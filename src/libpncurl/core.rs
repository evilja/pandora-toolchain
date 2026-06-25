use crate::{libpnenv::{
    core::get_env,
    standard::{
        CLIENT_ID, CLIENT_SECRET, PARENTID, REFRESH_TOKEN, TOKEN_URL, DOODSTREAM,
        LULU, VOESX, ABYSS,
    }
}, libpnlogging::core::LoggingHandle, log};
use reqwest::{Client, multipart};
use serde::Deserialize;
use std::{collections::HashMap, path::PathBuf, time::Duration};
use std::fs::File;
use tokio_util::io::ReaderStream;
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex, mpsc::Sender};
use tokio::io::{AsyncRead, ReadBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

const UPLOAD_TIMEOUT_SECS: u64 = 600;
const UPLOAD_TIMEOUT_EXTENSION_SECS: u64 = 100;
const UPLOAD_SPEED_CHECK_INTERVAL_SECS: u64 = 60;
const UPLOAD_SPEED_THRESHOLD_BYTES_PER_SEC: f64 = 3.0 * 1024.0 * 1024.0;
const UPLOAD_TIMEOUT_THRESHOLDS: [f64; 4] = [70.0, 85.0, 90.0, 95.0];

struct UploadProgress {
    sent: u64,
    total: u64,
}

impl UploadProgress {
    fn new(total: u64) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self { sent: 0, total }))
    }
}

struct ProgressReader<R> {
    inner: R,
    sent: u64,
    total: u64,
    tx: Sender<RpbData>,
    host: Host,
    progress: Arc<Mutex<UploadProgress>>,
}

impl<R> ProgressReader<R> {
    fn new(inner: R, total: u64, tx: Sender<RpbData>, host: Host, progress: Arc<Mutex<UploadProgress>>) -> Self {
        Self { inner, sent: 0, total, tx, host, progress }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        let after = buf.filled().len();

        let n = (after - before) as u64;
        if n > 0 {
            self.sent += n;
            if let Ok(mut progress) = self.progress.lock() {
                progress.sent = self.sent;
            }
            self.tx.send(RpbData::Progress(self.sent, self.total, self.host.clone())).ok();
        }

        result
    }
}

async fn send_upload_with_dynamic_timeout(
    request: reqwest::RequestBuilder,
    progress: Arc<Mutex<UploadProgress>>,
    base_timeout_secs: u64,
    label: &str,
) -> Result<reqwest::Response, String> {
    let mut request = Box::pin(request.send());
    let mut deadline = tokio::time::Instant::now() + Duration::from_secs(base_timeout_secs);
    let mut allowed_secs = base_timeout_secs;
    let mut threshold_idx = 0usize;
    let mut last_sent = 0u64;
    let mut last_check = std::time::Instant::now();

    loop {
        tokio::select! {
            result = &mut request => {
                return result.map_err(|e| e.to_string());
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(format!("dynamic upload timeout after {allowed_secs}s"));
            }
            _ = tokio::time::sleep(Duration::from_secs(UPLOAD_SPEED_CHECK_INTERVAL_SECS)) => {
                let (sent, total) = match progress.lock() {
                    Ok(progress) => (progress.sent, progress.total),
                    Err(_) => (last_sent, 0),
                };
                let elapsed = last_check.elapsed().as_secs_f64();
                let speed = if elapsed > 0.0 {
                    sent.saturating_sub(last_sent) as f64 / elapsed
                } else {
                    0.0
                };
                let percent = if total > 0 {
                    sent as f64 * 100.0 / total as f64
                } else {
                    0.0
                };
                let mut extended = false;

                while threshold_idx < UPLOAD_TIMEOUT_THRESHOLDS.len()
                    && percent >= UPLOAD_TIMEOUT_THRESHOLDS[threshold_idx]
                {
                    deadline = deadline + Duration::from_secs(UPLOAD_TIMEOUT_EXTENSION_SECS);
                    allowed_secs += UPLOAD_TIMEOUT_EXTENSION_SECS;
                    threshold_idx += 1;
                    extended = true;
                    println!("[upload-timeout] {label}: +{UPLOAD_TIMEOUT_EXTENSION_SECS}s ({percent:.2}%, {:.2}MB/s)", speed / 1_048_576.0);
                }

                if !extended
                    && threshold_idx < UPLOAD_TIMEOUT_THRESHOLDS.len()
                    && speed >= UPLOAD_SPEED_THRESHOLD_BYTES_PER_SEC
                {
                    deadline = deadline + Duration::from_secs(UPLOAD_TIMEOUT_EXTENSION_SECS);
                    allowed_secs += UPLOAD_TIMEOUT_EXTENSION_SECS;
                    threshold_idx += 1;
                    println!("[upload-timeout] {label}: +{UPLOAD_TIMEOUT_EXTENSION_SECS}s ({percent:.2}%, {:.2}MB/s)", speed / 1_048_576.0);
                }

                last_sent = sent;
                last_check = std::time::Instant::now();
            }
        }
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

async fn get_access_token(
    // CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, TOKEN_URL
    env: &HashMap<String, String>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    let params = [
        ("client_id", env.get(CLIENT_ID).cloned().unwrap_or_default()),
        ("client_secret", env.get(CLIENT_SECRET).cloned().unwrap_or_default()),
        ("refresh_token", env.get(REFRESH_TOKEN).cloned().unwrap_or_default()),
        ("grant_type", "refresh_token".into()),
    ];

    let resp = client
        .post(env.get(TOKEN_URL).cloned().unwrap_or_default())
        .form(&params)
        .send().await?
        .error_for_status()?;

    let token: TokenResponse = resp.json().await?;

    Ok(token.access_token)
}

#[derive(Clone, Debug)]
pub enum Host {
    Drive,
    Doodstream,
    Lulu,
    VoeSx,
    Abyss,
}
pub enum RpbData {
    Progress(u64, u64, Host),
    Done(String, Host),
    Fail(Host),
}

pub struct Req {
    pub target: String,
    pub log: Option<PathBuf>,
}

impl Req {
    pub async fn send(&self, path: String) -> bool {
        match self.download(&path).await {
            Ok(_) => true,
            Err(_) => false,
        }
    }

    async fn download(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let mut handle: Option<LoggingHandle> = match self.log {
            Some(ref pb) => {
                Some(LoggingHandle::get_handle(pb).await.unwrap())
            }
            None => None,
        };
        let client = Client::builder()
            .timeout(Duration::from_secs(600))
            .build()?;
        let response = client.get(&self.target).send().await?;

        if !response.status().is_success() {
            log!(handle, "Request failed\n");
            return Err("Request failed".into());
        }
        log!(handle, "Request succeeded\n");

        let bytes = match response.bytes().await {
            Ok(b) => {
                log!(handle, "Byte conversion succeeded\n");
                b
            }
            Err(a) => {
                log!(handle, &format!("Byte conversion failed: {a}\n"));
                return Err(a.into());
            }
        };

        let mut file = match File::create(Path::new(path)) {
            Ok(f) => {
                log!(handle, &format!("File created: {path}\n"));
                f
            }
            Err(a) => {
                log!(handle, &format!("File creation failed: {a}\n"));
                return Err(a.into());
            }
        };
        file.write_all(&bytes).unwrap();
        if let Some(mut a) = handle {
            a.flush().await;
        }
        Ok(())
    }

    async fn filehost_upload(
        &self,
        api_key: String,
        server_endpoint: String,
        key_field: &str,        // "api_key" for dood/uq, "key" for lulu
        link_fn: impl Fn(&str) -> String,
        host: Host,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        println!("HOST {:?}", host);
        // Step 1: get upload server
        let server_url = {
            let resp = match reqwest::get(
                format!("{server_endpoint}?key={api_key}")
            ).await {
                Ok(r) => r,
                Err(a) => {
                    println!("[upload] failed to get upload server: {a}");
                    tx.send(RpbData::Fail(host)).ok();
                    return false;
                }
            };

            let json: serde_json::Value = match resp.json().await {
                Ok(j) => { println!("[upload] server response: {j}"); j },
                Err(a) => {
                    println!("[upload] failed to parse server response: {a}");
                    tx.send(RpbData::Fail(host)).ok();
                    return false;
                }
            };

            match json["result"].as_str() {
                Some(url) => { println!("[upload] upload server url: {url}"); url.to_string() },
                None => {
                    println!("[upload] no upload server url in response");
                    tx.send(RpbData::Fail(host)).ok();
                    return false;
                }
            }
        };

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(60))
            .build().unwrap();

        let upload_name = outfile.unwrap_or(self.target.clone());
        println!("[upload] upload_name: {upload_name}, target: {}", self.target);

        let file = match tokio::fs::File::open(&self.target).await {
            Ok(f) => { println!("[upload] file opened"); f },
            Err(a) => {
                println!("[upload] failed to open file: {a}");
                tx.send(RpbData::Fail(host.clone())).ok();
                return false;
            }
        };

        let total_size = file.metadata().await.unwrap().len();
        println!("[upload] total_size: {total_size} bytes ({:.2}MB)", total_size as f64 / 1_048_576.0);

        let progress = UploadProgress::new(total_size);
        let reader = ProgressReader::new(file, total_size, tx.clone(), host.clone(), progress.clone());
        let stream = ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);

        let file_part = multipart::Part::stream_with_length(body, total_size)
            .file_name(upload_name.clone())
            .mime_str("video/mp4")
            .unwrap();

        let form = multipart::Form::new()
            .text(key_field.to_string(), api_key.clone())
            .part("file", file_part);

        println!("[upload] sending to {server_url}...");
        let host_label = format!("{:?}", host);
        let resp = match send_upload_with_dynamic_timeout(
            client.post(&server_url).multipart(form),
            progress,
            UPLOAD_TIMEOUT_SECS,
            &host_label,
        ).await {
            Ok(r) => { println!("[upload] response status: {}", r.status()); r },
            Err(a) => {
                println!("[upload] request failed: {a}");
                tx.send(RpbData::Fail(host)).ok();
                return false;
            }
        };

        // each host parses response differently, use closure
        let link = match link_fn(&resp.text().await.unwrap_or_default()) {
            s if s.is_empty() => {
                println!("[upload] failed to extract link");
                tx.send(RpbData::Fail(host)).ok();
                return false;
            },
            s => s,
        };

        println!("[upload] link: {link}");
        tx.send(RpbData::Done(link, host)).ok();
        true
    }

    pub async fn voewrapupload(&self, envpath: String, outfile: Option<String>, tx: Sender<RpbData>) -> bool {
        let env = get_env(&envpath);
        let api_key = env.get(VOESX).cloned().unwrap_or_default();
        let result = self.filehost_upload(
            api_key,
            "https://voe.sx/api/upload/server".to_string(),
            "key",
            |text| {
                serde_json::from_str::<serde_json::Value>(text).ok()
                    .and_then(|j| j["file"]["file_code"].as_str().map(|s| format!("https://voe.sx/{s}")))
                    .unwrap_or_default()
            },
            Host::VoeSx,
            outfile,
            tx,
        ).await;
        result
    }

    pub async fn abyssupload(&self, envpath: String, outfile: Option<String>, tx: Sender<RpbData>) -> bool {
        println!("[abyss] abyssupload started");
        let env = get_env(&envpath);
        println!("[abyss] env len: {}, ABYSS key: {}, value: {:?}", env.len(), ABYSS, env.get(ABYSS));
        let api_key = env.get(ABYSS).cloned().unwrap_or_default();

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(60))
            .build().unwrap();

        let upload_name = outfile.unwrap_or(self.target.clone());
        println!("[abyss] upload_name: {upload_name}, target: {}", self.target);

        let file = match tokio::fs::File::open(&self.target).await {
            Ok(f) => { println!("[abyss] file opened"); f },
            Err(a) => {
                println!("[abyss] failed to open file: {a}");
                tx.send(RpbData::Fail(Host::Abyss)).ok();
                return false;
            }
        };

        let total_size = file.metadata().await.unwrap().len();
        println!("[abyss] total_size: {total_size} bytes ({:.2}MB)", total_size as f64 / 1_048_576.0);

        let progress = UploadProgress::new(total_size);
        let reader = ProgressReader::new(file, total_size, tx.clone(), Host::Abyss, progress.clone());
        let stream = ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);

        let file_part = multipart::Part::stream_with_length(body, total_size)
            .file_name(upload_name.clone())
            .mime_str("video/mp4")
            .unwrap();

        let form = multipart::Form::new()
            .part("file", file_part);

        let upload_url = format!("https://up.abyss.to/{api_key}");
        println!("[abyss] uploading to {upload_url}...");

        let resp = match send_upload_with_dynamic_timeout(
            client.post(&upload_url).multipart(form),
            progress,
            UPLOAD_TIMEOUT_SECS,
            "Abyss",
        ).await {
            Ok(r) => { println!("[abyss] response status: {}", r.status()); r },
            Err(a) => {
                println!("[abyss] request failed: {a}");
                tx.send(RpbData::Fail(Host::Abyss)).ok();
                return false;
            }
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => { println!("[abyss] response: {j}"); j },
            Err(a) => {
                println!("[abyss] failed to parse response: {a}");
                tx.send(RpbData::Fail(Host::Abyss)).ok();
                return false;
            }
        };

        let link = json["urlIframe"].as_str()
            .map(|s| s.to_string())
            .or_else(|| json["slug"].as_str().map(|s| format!("https://abyss.to/r/{s}")))
            .unwrap_or_default();

        if link.is_empty() {
            println!("[abyss] no link in response: {json}");
            tx.send(RpbData::Fail(Host::Abyss)).ok();
            return false;
        }
        tx.send(RpbData::Done(link, Host::Abyss)).ok();
        true
    }

    pub async fn luluwrapupload(&self, envpath: String, outfile: Option<String>, tx: Sender<RpbData>) -> bool {
        let env = get_env(&envpath);
        let api_key = env.get(LULU).cloned().unwrap_or_default();
        let result = self.filehost_upload(
            api_key,
            "https://lulustream.com/api/upload/server".to_string(),
            "key",
            |text| {
                println!("[lulu] upload response: {text}");
                serde_json::from_str::<serde_json::Value>(text).ok()
                    .and_then(|j| j["files"][0]["filecode"].as_str().map(|s| format!("https://lulustream.com/{s}")))
                    .unwrap_or_default()
            },
            Host::Lulu,
            outfile,
            tx,
        ).await;
        result
    }

    pub async fn doodwrapupload(&self, envpath: String, outfile: Option<String>, tx: Sender<RpbData>) -> bool {
        let env = get_env(&envpath);
        let api_key = env.get(DOODSTREAM).cloned().unwrap_or_default();
        self.filehost_upload(
            api_key,
            "https://doodapi.co/api/upload/server".to_string(),
            "api_key",
            |text| {
                println!("[dood] upload response: {text}");
                serde_json::from_str::<serde_json::Value>(text).ok()
                    .and_then(|j| j["result"][0]["filecode"].as_str().map(|s| format!("https://doodstream.com/d/{s}")))
                    .unwrap_or_default()
            },
            Host::Doodstream,
            outfile,
            tx,
        ).await
    }


    pub async fn gdupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        let mut handle: Option<LoggingHandle> = match self.log {
            Some(ref pb) => {
                Some(LoggingHandle::get_handle(pb).await.unwrap())
            }
            None => None,
        };
        let env = get_env(&envpath);
        let access_token = match get_access_token(&env).await {
            Ok(token) => {
                log!(handle, "Access token taken\n");
                token
            },
            Err(_) => {
                return false;
            },
        };
        let parent_id = env.get(PARENTID).cloned().unwrap_or_default();

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(60))
            .build().unwrap();

        let upload_name = outfile.clone().unwrap_or(self.target.clone());

        let metadata = serde_json::json!({
            "name": upload_name,
            "parents": [parent_id],
        });

        let file = match tokio::fs::File::open(&self.target).await {
            Ok(f) => {
                log!(handle, &format!("File created: {}\n", &self.target));
                f
            },
            Err(a) => {
                log!(handle, &format!("File creation failed: {a}\n"));
                return false
            }
        };

        let total_size = file.metadata().await.unwrap().len();

        let upload_name = outfile.clone().unwrap_or(self.target.clone());

        let progress = UploadProgress::new(total_size);
        let reader = ProgressReader::new(file, total_size, tx.clone(), Host::Drive, progress.clone());

        let stream = ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);

        let part = multipart::Part::stream(body)
            .file_name(upload_name.clone())
            .mime_str("video/mp4")
            .unwrap();

        let metadata_part = multipart::Part::text(metadata.to_string())
            .mime_str("application/json; charset=UTF-8")
            .unwrap();

        let form = multipart::Form::new()
            .part("metadata", metadata_part)
            .part("file", part);


        let resp = match send_upload_with_dynamic_timeout(
            client
                .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&supportsAllDrives=true")
                .bearer_auth(access_token)
                .multipart(form),
            progress,
            UPLOAD_TIMEOUT_SECS,
            "Drive",
        ).await {
            Ok(r) => r,
            Err(a) => {
                println!("{a}");
                tx.send(RpbData::Fail(Host::Drive)).ok();
                return false;
            }
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => j,
            Err(a) => {
                println!("{:?}", a);
                tx.send(RpbData::Fail(Host::Drive)).ok();
                return false;
            }
        };

        let file_id = match json["id"].as_str() {
            Some(id) => id,
            None => {
                tx.send(RpbData::Fail(Host::Drive)).ok();
                return false;
            }
        };

        let link = format!(
            "https://drive.google.com/file/d/{}/view?usp=sharing",
            file_id
        );
        log!(handle, &(link.clone() + "\n"));
        if let Some(mut a) = handle {
            a.flush().await;
        }
        tx.send(RpbData::Done(link.clone(), Host::Drive)).ok();
        true
    }
}
