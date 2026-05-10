use crate::{libpnenv::{
    core::get_env,
    standard::{
        CLIENT_ID, CLIENT_SECRET, PARENTID, REFRESH_TOKEN, TOKEN_URL, DOODSTREAM,
        UQLOAD, LULU, VOESX, ABYSS,
    }
}, libpnlogging::core::LoggingHandle, log};
use reqwest::{Client, multipart};
use serde::Deserialize;
use std::{path::PathBuf, time::Duration};
use std::fs::File;
use tokio_util::io::ReaderStream;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::Sender;
use tokio::io::{AsyncRead, ReadBuf};
use std::pin::Pin;
use std::task::{Context, Poll};

struct ProgressReader<R> {
    inner: R,
    sent: u64,
    total: u64,
    tx: Sender<RpbData>,
    host: Host,
}

impl<R> ProgressReader<R> {
    fn new(inner: R, total: u64, tx: Sender<RpbData>, host: Host) -> Self {
        Self { inner, sent: 0, total, tx, host }
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
            self.tx.send(RpbData::Progress(self.sent, self.total, self.host.clone())).ok();
        }

        result
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

async fn get_access_token(
    // CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, TOKEN_URL
    env: &Vec<String>,
) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let client = Client::new();

    let params = [
        ("client_id", env[CLIENT_ID].clone()),
        ("client_secret", env[CLIENT_SECRET].clone()),
        ("refresh_token", env[REFRESH_TOKEN].clone()),
        ("grant_type", "refresh_token".into()),
    ];

    let resp = client
        .post(env[TOKEN_URL].clone())
        .form(&params)
        .send().await?
        .error_for_status()?;

    let token: TokenResponse = resp.json().await?;

    Ok(token.access_token)
}

#[derive(Clone)]
pub enum Host {
    Drive,
    Doodstream,
    Uqload,
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
        let response = reqwest::get(&self.target).await?;

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
            .timeout(Duration::from_secs(360))
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

        let reader = ProgressReader::new(file, total_size, tx.clone(), host.clone());
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
        let resp = match client.post(&server_url).multipart(form).send().await {
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
        let api_key = env[VOESX].clone();
        let result = self.filehost_upload(
            api_key,
            "https://voe.sx/api/upload/server".to_string(),
            "key",
            |text| {
                serde_json::from_str::<serde_json::Value>(text).ok()
                    .and_then(|j| j["file"]["file_code"].as_str().map(|s| format!("https://voe.sx/e/{s}")))
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
        println!("[abyss] env len: {}, ABYSS idx: {}, value: {:?}", env.len(), ABYSS, env.get(ABYSS));
        let api_key = env[ABYSS].clone();

        let client = Client::builder()
            .timeout(Duration::from_secs(360))
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

        let reader = ProgressReader::new(file, total_size, tx.clone(), Host::Abyss);
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

        let resp = match client.post(&upload_url).multipart(form).send().await {
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
        let api_key = env[LULU].clone();
        let result = self.filehost_upload(
            api_key,
            "https://lulustream.com/api/upload/server".to_string(),
            "key",
            |text| {
                serde_json::from_str::<serde_json::Value>(text).ok()
                    .and_then(|j| j["files"][0]["filecode"].as_str().map(|s| format!("https://lulustream.com/e/{s}")))
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
        let api_key = env[DOODSTREAM].clone();
        self.filehost_upload(
            api_key,
            "https://doodapi.co/api/upload/server".to_string(),
            "api_key",
            |text| {
                // JSON response
                serde_json::from_str::<serde_json::Value>(text).ok()
                    .and_then(|j| j["result"][0]["download_url"].as_str().map(|s| s.to_string()))
                    .unwrap_or_default()
            },
            Host::Doodstream,
            outfile,
            tx,
        ).await
    }

    pub async fn uqwrapupload(&self, envpath: String, outfile: Option<String>, tx: Sender<RpbData>) -> bool {
        let env = get_env(&envpath);
        let api_key = env[UQLOAD].clone();
        self.filehost_upload(
            api_key,
            "https://uqload.is/api/upload/server".to_string(),
            "api_key",
            |text| {
                // HTML response
                text.split(r#"name="fn">"#)
                    .nth(1)
                    .and_then(|s| s.split("</textarea>").next())
                    .map(|code| format!("https://uqload.is/{code}"))
                    .unwrap_or_default()
            },
            Host::Uqload,
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
        let parent_id = env[PARENTID].clone();

        let client = Client::builder()
            .timeout(Duration::from_secs(360))
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

        let reader = ProgressReader::new(file, total_size, tx.clone(), Host::Drive);

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


        let resp = match client
            .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&supportsAllDrives=true")
            .bearer_auth(access_token)
            .multipart(form)
            .send().await
        {
            Ok(r) => r,
            Err(a) => {
                println!("{:?}", a);
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
