use crate::{
    libpnenv::{
        core::get_env,
        standard::{
            ABYSS, CLIENT_ID, CLIENT_SECRET, DOODSTREAM, LULU, PARENTID, REFRESH_TOKEN, TOKEN_URL,
            VOESX,
        },
    },
    libpnlogging::core::LoggingHandle,
    log,
};
use reqwest::{Client, multipart};
use serde::Deserialize;
use std::fs::File;
use std::io::{Error, ErrorKind, Write};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, mpsc::Sender};
use std::task::{Context, Poll};
use std::{collections::HashMap, path::PathBuf, time::Duration};
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::io::ReaderStream;

const DRIVE_FOLDER_MIME: &str = "application/vnd.google-apps.folder";

struct UploadProgress {
    sent: u64,
    extensions: u64,
}

impl UploadProgress {
    fn new(_total: u64) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self {
            sent: 0,
            extensions: 0,
        }))
    }
}

struct ProgressReader<R> {
    inner: R,
    sent: u64,
    total: u64,
    tx: Sender<RpbData>,
    host: Host,
    progress: Arc<Mutex<UploadProgress>>,
    cfile: Option<PathBuf>,
}

impl<R> ProgressReader<R> {
    fn new(
        inner: R,
        total: u64,
        tx: Sender<RpbData>,
        host: Host,
        progress: Arc<Mutex<UploadProgress>>,
        cfile: Option<PathBuf>,
    ) -> Self {
        Self {
            inner,
            sent: 0,
            total,
            tx,
            host,
            progress,
            cfile,
        }
    }
}

impl<R: AsyncRead + Unpin> AsyncRead for ProgressReader<R> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        if is_cancelled(&self.cfile) {
            self.tx.send(RpbData::Cancel(self.host.clone())).ok();
            return Poll::Ready(Err(Error::new(ErrorKind::Interrupted, "cancelled")));
        }
        let before = buf.filled().len();
        let result = Pin::new(&mut self.inner).poll_read(cx, buf);
        let after = buf.filled().len();

        let n = (after - before) as u64;
        if n > 0 {
            self.sent += n;
            if let Ok(mut progress) = self.progress.lock() {
                progress.sent = self.sent;
            }
            let extensions = match self.progress.lock() {
                Ok(progress) => progress.extensions,
                Err(_) => 0,
            };
            self.tx
                .send(RpbData::Progress(
                    self.sent,
                    self.total,
                    extensions,
                    self.host.clone(),
                ))
                .ok();
        }

        result
    }
}

async fn send_upload_unlimited(
    request: reqwest::RequestBuilder,
) -> Result<reqwest::Response, String> {
    request.send().await.map_err(|e| e.to_string())
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
        (
            "client_secret",
            env.get(CLIENT_SECRET).cloned().unwrap_or_default(),
        ),
        (
            "refresh_token",
            env.get(REFRESH_TOKEN).cloned().unwrap_or_default(),
        ),
        ("grant_type", "refresh_token".into()),
    ];

    let resp = client
        .post(env.get(TOKEN_URL).cloned().unwrap_or_default())
        .form(&params)
        .send()
        .await?
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
    Progress(u64, u64, u64, Host),
    Done(String, Host),
    Fail(Host),
    Cancel(Host),
}

pub struct Req {
    pub target: String,
    pub log: Option<PathBuf>,
    pub cfile: Option<PathBuf>,
}

fn is_cancelled(cfile: &Option<PathBuf>) -> bool {
    cfile.as_ref().map(|p| p.exists()).unwrap_or(false)
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
            Some(ref pb) => Some(LoggingHandle::get_handle(pb).await.unwrap()),
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
        key_field: &str, // "api_key" for dood/uq, "key" for lulu
        link_fn: impl Fn(&str) -> String,
        host: Host,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        if is_cancelled(&self.cfile) {
            tx.send(RpbData::Cancel(host)).ok();
            return false;
        }
        println!("HOST {:?}", host);
        // Step 1: get upload server
        let server_url = {
            let resp = match reqwest::get(format!("{server_endpoint}?key={api_key}")).await {
                Ok(r) => r,
                Err(a) => {
                    println!("[upload] failed to get upload server: {a}");
                    tx.send(RpbData::Fail(host)).ok();
                    return false;
                }
            };

            let json: serde_json::Value = match resp.json().await {
                Ok(j) => {
                    println!("[upload] server response: {j}");
                    j
                }
                Err(a) => {
                    println!("[upload] failed to parse server response: {a}");
                    tx.send(RpbData::Fail(host)).ok();
                    return false;
                }
            };

            match json["result"].as_str() {
                Some(url) => {
                    println!("[upload] upload server url: {url}");
                    url.to_string()
                }
                None => {
                    println!("[upload] no upload server url in response");
                    tx.send(RpbData::Fail(host)).ok();
                    return false;
                }
            }
        };

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(60))
            .build()
            .unwrap();

        let upload_name = outfile.unwrap_or(self.target.clone());
        println!(
            "[upload] upload_name: {upload_name}, target: {}",
            self.target
        );

        let file = match tokio::fs::File::open(&self.target).await {
            Ok(f) => {
                println!("[upload] file opened");
                f
            }
            Err(a) => {
                println!("[upload] failed to open file: {a}");
                tx.send(RpbData::Fail(host.clone())).ok();
                return false;
            }
        };

        let total_size = file.metadata().await.unwrap().len();
        println!(
            "[upload] total_size: {total_size} bytes ({:.2}MB)",
            total_size as f64 / 1_048_576.0
        );

        let progress = UploadProgress::new(total_size);
        let reader =
            ProgressReader::new(file, total_size, tx.clone(), host.clone(), progress.clone(), self.cfile.clone());
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
        let resp = match send_upload_unlimited(client.post(&server_url).multipart(form)).await {
            Ok(r) => {
                println!("[upload] response status: {}", r.status());
                r
            }
            Err(a) => {
                println!("[upload] request failed: {a}");
                if is_cancelled(&self.cfile) {
                    tx.send(RpbData::Cancel(host)).ok();
                    return false;
                }
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
            }
            s => s,
        };

        println!("[upload] link: {link}");
        tx.send(RpbData::Done(link, host)).ok();
        true
    }

    pub async fn voewrapupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        let env = get_env(&envpath);
        let api_key = env.get(VOESX).cloned().unwrap_or_default();
        let result = self
            .filehost_upload(
                api_key,
                "https://voe.sx/api/upload/server".to_string(),
                "key",
                |text| {
                    serde_json::from_str::<serde_json::Value>(text)
                        .ok()
                        .and_then(|j| {
                            j["file"]["file_code"]
                                .as_str()
                                .map(|s| format!("https://voe.sx/{s}"))
                        })
                        .unwrap_or_default()
                },
                Host::VoeSx,
                outfile,
                tx,
            )
            .await;
        result
    }

    pub async fn abyssupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        if is_cancelled(&self.cfile) {
            tx.send(RpbData::Cancel(Host::Abyss)).ok();
            return false;
        }
        println!("[abyss] abyssupload started");
        let env = get_env(&envpath);
        println!(
            "[abyss] env len: {}, ABYSS key: {}, value: {:?}",
            env.len(),
            ABYSS,
            env.get(ABYSS)
        );
        let api_key = env.get(ABYSS).cloned().unwrap_or_default();

        let client = Client::builder()
            .connect_timeout(Duration::from_secs(60))
            .build()
            .unwrap();

        let upload_name = outfile.unwrap_or(self.target.clone());
        println!(
            "[abyss] upload_name: {upload_name}, target: {}",
            self.target
        );

        let file = match tokio::fs::File::open(&self.target).await {
            Ok(f) => {
                println!("[abyss] file opened");
                f
            }
            Err(a) => {
                println!("[abyss] failed to open file: {a}");
                tx.send(RpbData::Fail(Host::Abyss)).ok();
                return false;
            }
        };

        let total_size = file.metadata().await.unwrap().len();
        println!(
            "[abyss] total_size: {total_size} bytes ({:.2}MB)",
            total_size as f64 / 1_048_576.0
        );

        let progress = UploadProgress::new(total_size);
        let reader =
            ProgressReader::new(file, total_size, tx.clone(), Host::Abyss, progress.clone(), self.cfile.clone());
        let stream = ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);

        let file_part = multipart::Part::stream_with_length(body, total_size)
            .file_name(upload_name.clone())
            .mime_str("video/mp4")
            .unwrap();

        let form = multipart::Form::new().part("file", file_part);

        let upload_url = format!("https://up.abyss.to/{api_key}");
        println!("[abyss] uploading to {upload_url}...");

        let resp = match send_upload_unlimited(client.post(&upload_url).multipart(form)).await {
            Ok(r) => {
                println!("[abyss] response status: {}", r.status());
                r
            }
            Err(a) => {
                println!("[abyss] request failed: {a}");
                if is_cancelled(&self.cfile) {
                    tx.send(RpbData::Cancel(Host::Abyss)).ok();
                    return false;
                }
                tx.send(RpbData::Fail(Host::Abyss)).ok();
                return false;
            }
        };

        let json: serde_json::Value = match resp.json().await {
            Ok(j) => {
                println!("[abyss] response: {j}");
                j
            }
            Err(a) => {
                println!("[abyss] failed to parse response: {a}");
                tx.send(RpbData::Fail(Host::Abyss)).ok();
                return false;
            }
        };

        let link = json["urlIframe"]
            .as_str()
            .map(|s| s.to_string())
            .or_else(|| {
                json["slug"]
                    .as_str()
                    .map(|s| format!("https://abyss.to/r/{s}"))
            })
            .unwrap_or_default();

        if link.is_empty() {
            println!("[abyss] no link in response: {json}");
            tx.send(RpbData::Fail(Host::Abyss)).ok();
            return false;
        }
        tx.send(RpbData::Done(link, Host::Abyss)).ok();
        true
    }

    pub async fn luluwrapupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        let env = get_env(&envpath);
        let api_key = env.get(LULU).cloned().unwrap_or_default();
        let result = self
            .filehost_upload(
                api_key,
                "https://lulustream.com/api/upload/server".to_string(),
                "key",
                |text| {
                    println!("[lulu] upload response: {text}");
                    serde_json::from_str::<serde_json::Value>(text)
                        .ok()
                        .and_then(|j| {
                            j["files"][0]["filecode"]
                                .as_str()
                                .map(|s| format!("https://lulustream.com/{s}"))
                        })
                        .unwrap_or_default()
                },
                Host::Lulu,
                outfile,
                tx,
            )
            .await;
        result
    }

    pub async fn doodwrapupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        let env = get_env(&envpath);
        let api_key = env.get(DOODSTREAM).cloned().unwrap_or_default();
        self.filehost_upload(
            api_key,
            "https://doodapi.co/api/upload/server".to_string(),
            "api_key",
            |text| {
                println!("[dood] upload response: {text}");
                serde_json::from_str::<serde_json::Value>(text)
                    .ok()
                    .and_then(|j| {
                        j["result"][0]["filecode"]
                            .as_str()
                            .map(|s| format!("https://doodstream.com/d/{s}"))
                    })
                    .unwrap_or_default()
            },
            Host::Doodstream,
            outfile,
            tx,
        )
        .await
    }

    pub async fn gdupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        drive_folder: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        if is_cancelled(&self.cfile) {
            tx.send(RpbData::Cancel(Host::Drive)).ok();
            return false;
        }
        let mut handle: Option<LoggingHandle> = match self.log {
            Some(ref pb) => Some(LoggingHandle::get_handle(pb).await.unwrap()),
            None => None,
        };
        let env = get_env(&envpath);
        let access_token = match get_access_token(&env).await {
            Ok(token) => {
                log!(handle, "Access token taken\n");
                token
            }
            Err(_) => {
                return false;
            }
        };
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(60))
            .build()
            .unwrap();

        let mut parent_id = env.get(PARENTID).cloned().unwrap_or_default();
        if let Some(folder_path) = drive_folder.as_deref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
            match ensure_drive_folder_path(&client, &access_token, &parent_id, folder_path, &mut handle).await {
                Ok(id) => parent_id = id,
                Err(e) => {
                    println!("{e}");
                    tx.send(RpbData::Fail(Host::Drive)).ok();
                    return false;
                }
            }
        }

        let upload_name = outfile.clone().unwrap_or(self.target.clone());

        let metadata = serde_json::json!({
            "name": upload_name,
            "parents": [parent_id],
        });

        let file = match tokio::fs::File::open(&self.target).await {
            Ok(f) => {
                log!(handle, &format!("File created: {}\n", &self.target));
                f
            }
            Err(a) => {
                log!(handle, &format!("File creation failed: {a}\n"));
                return false;
            }
        };

        let total_size = file.metadata().await.unwrap().len();

        let upload_name = outfile.clone().unwrap_or(self.target.clone());

        let progress = UploadProgress::new(total_size);
        let reader =
            ProgressReader::new(file, total_size, tx.clone(), Host::Drive, progress.clone(), self.cfile.clone());

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

        let resp = match send_upload_unlimited(
            client
                .post("https://www.googleapis.com/upload/drive/v3/files?uploadType=multipart&supportsAllDrives=true")
                .bearer_auth(access_token)
                .multipart(form),
        ).await {
            Ok(r) => r,
            Err(a) => {
                println!("{a}");
                if is_cancelled(&self.cfile) {
                    tx.send(RpbData::Cancel(Host::Drive)).ok();
                    return false;
                }
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

async fn ensure_drive_folder_path(
    client: &Client,
    access_token: &str,
    root_parent_id: &str,
    folder_path: &str,
    handle: &mut Option<LoggingHandle>,
) -> Result<String, String> {
    let mut parent_id = root_parent_id.to_string();
    for name in folder_path.split('/').map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let next = ensure_drive_folder(client, access_token, &parent_id, name).await?;
        if let Some(h) = handle.as_mut() {
            h.write(&format!("Drive folder {name}: {next}\n")).await;
        }
        parent_id = next;
    }
    Ok(parent_id)
}

async fn ensure_drive_folder(
    client: &Client,
    access_token: &str,
    parent_id: &str,
    name: &str,
) -> Result<String, String> {
    let query = format!(
        "mimeType='{}' and trashed=false and name='{}' and '{}' in parents",
        DRIVE_FOLDER_MIME,
        drive_query_escape(name),
        drive_query_escape(parent_id),
    );
    let found: serde_json::Value = client
        .get("https://www.googleapis.com/drive/v3/files")
        .bearer_auth(access_token)
        .query(&[
            ("q", query.as_str()),
            ("fields", "files(id,name)"),
            ("pageSize", "1"),
            ("supportsAllDrives", "true"),
            ("includeItemsFromAllDrives", "true"),
        ])
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    if let Some(id) = found["files"]
        .as_array()
        .and_then(|files| files.first())
        .and_then(|file| file["id"].as_str())
    {
        return Ok(id.to_string());
    }

    let metadata = serde_json::json!({
        "name": name,
        "mimeType": DRIVE_FOLDER_MIME,
        "parents": [parent_id],
    });
    let created: serde_json::Value = client
        .post("https://www.googleapis.com/drive/v3/files")
        .bearer_auth(access_token)
        .query(&[("supportsAllDrives", "true"), ("fields", "id")])
        .json(&metadata)
        .send()
        .await
        .map_err(|e| e.to_string())?
        .error_for_status()
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    created["id"]
        .as_str()
        .map(|id| id.to_string())
        .ok_or_else(|| format!("Drive folder create response did not include an id for {name}"))
}

fn drive_query_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}
