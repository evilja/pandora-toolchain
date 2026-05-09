use crate::{libpnenv::{
    core::get_env,
    standard::{
        CLIENT_ID, CLIENT_SECRET, PARENTID, REFRESH_TOKEN, TOKEN_URL, DOODSTREAM
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

    pub async fn doodupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        tx: Sender<RpbData>,
    ) -> bool {
        println!("[dood] doodupload started");
        let mut handle: Option<LoggingHandle> = match self.log {
            Some(ref pb) => Some(LoggingHandle::get_handle(pb).await.unwrap()),
            None => None,
        };
        println!("[dood] log handle acquired: {}", self.log.is_some());

        let env = get_env(&envpath);
        let api_key = env[DOODSTREAM].clone();
        println!("[dood] api_key: {api_key}");

        println!("[dood] fetching upload server...");
        let server_url = {
            let resp = match reqwest::get(
                format!("https://doodapi.co/api/upload/server?key={api_key}")
            ).await {
                Ok(r) => {
                    println!("[dood] upload server response status: {}", r.status());
                    r
                },
                Err(a) => {
                    println!("[dood] failed to get upload server: {a}");
                    log!(handle, &format!("Failed to get upload server: {a}\n"));
                    tx.send(RpbData::Fail(Host::Doodstream)).ok();
                    return false;
                }
            };

            let json: serde_json::Value = match resp.json().await {
                Ok(j) => {
                    println!("[dood] server response json: {j}");
                    j
                },
                Err(a) => {
                    println!("[dood] failed to parse server response: {a}");
                    log!(handle, &format!("Failed to parse server response: {a}\n"));
                    tx.send(RpbData::Fail(Host::Doodstream)).ok();
                    return false;
                }
            };

            match json["result"].as_str() {
                Some(url) => {
                    println!("[dood] upload server url: {url}");
                    url.to_string()
                },
                None => {
                    println!("[dood] no upload server url in response");
                    log!(handle, "No upload server URL in response\n");
                    tx.send(RpbData::Fail(Host::Doodstream)).ok();
                    return false;
                }
            }
        };
        log!(handle, &format!("Upload server: {server_url}\n"));

        println!("[dood] building http client...");
        let client = Client::builder()
            .timeout(Duration::from_secs(360))
            .build().unwrap();

        let upload_name = outfile.unwrap_or(self.target.clone());
        println!("[dood] upload_name: {upload_name}");
        println!("[dood] target file: {}", self.target);

        println!("[dood] opening file...");
        let file = match tokio::fs::File::open(&self.target).await {
            Ok(f) => {
                println!("[dood] file opened successfully");
                log!(handle, &format!("Opened file: {}\n", &self.target));
                f
            }
            Err(a) => {
                println!("[dood] failed to open file: {a}");
                log!(handle, &format!("Failed to open file: {a}\n"));
                tx.send(RpbData::Fail(Host::Doodstream)).ok();
                return false;
            }
        };

        let total_size = file.metadata().await.unwrap().len();
        println!("[dood] total_size: {total_size} bytes ({:.2}MB)", total_size as f64 / 1_048_576.0);

        println!("[dood] setting up progress reader and stream...");
        let reader = ProgressReader::new(file, total_size, tx.clone(), Host::Doodstream);
        let stream = ReaderStream::new(reader);
        let body = reqwest::Body::wrap_stream(stream);

        println!("[dood] building multipart form...");
        let file_part = multipart::Part::stream_with_length(body, total_size)
            .file_name(upload_name)
            .mime_str("video/mp4")
            .unwrap();
        let form = multipart::Form::new()
            .text("api_key", api_key.clone())
            .part("file", file_part);

        let upload_url = format!("{server_url}");
        println!("[dood] upload_url: {upload_url}");

        println!("[dood] sending upload request...");
        let resp = match client.post(&upload_url).multipart(form).send().await {
            Ok(r) => {
                println!("[dood] upload response status: {}", r.status());
                r
            },
            Err(a) => {
                println!("[dood] upload request failed: {a}");
                log!(handle, &format!("Upload request failed: {a}\n"));
                tx.send(RpbData::Fail(Host::Doodstream)).ok();
                return false;
            }
        };

        println!("[dood] parsing upload response json...");
        let json: serde_json::Value = match resp.json().await {
            Ok(j) => {
                println!("[dood] upload response json: {j}");
                j
            },
            Err(a) => {
                println!("[dood] failed to parse upload response: {a}");
                log!(handle, &format!("Failed to parse upload response: {a}\n"));
                tx.send(RpbData::Fail(Host::Doodstream)).ok();
                return false;
            }
        };

        let download_url = match json["result"][0]["download_url"].as_str() {
            Some(url) => {
                println!("[dood] download_url: {url}");
                url.to_string()
            },
            None => {
                println!("[dood] no download_url in response: {json}");
                log!(handle, &format!("No download_url in response: {json}\n"));
                tx.send(RpbData::Fail(Host::Doodstream)).ok();
                return false;
            }
        };

        println!("[dood] upload complete, sending Done");
        log!(handle, &(download_url.clone() + "\n"));
        if let Some(mut a) = handle {
            a.flush().await;
        }
        tx.send(RpbData::Done(download_url, Host::Doodstream)).ok();
        println!("[dood] doodupload finished successfully");
        true
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
