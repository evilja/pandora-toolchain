// https://drive.usercontent.google.com/download?id=1QMul1d30l_ux05JJW5ZD8V4H8TWnXJ1S&export=download
// https://drive.google.com/file/d/1QMul1d30l_ux05JJW5ZD8V4H8TWnXJ1S/view

// https://drive.usercontent.google.com/download?id=1QMul1d30l_ux05JJW5ZD8V4H8TWnXJ1S&export=download&confirm=t&uuid=4508405e-7311-498a-a81c-77bd9ee5f5f7

use crate::{lib_pn_data, lib_pn_emit, lib_pn_schema, libpnlogging::core::LoggingHandle, libpnprotocol::core::{Protocol, Schema}, log};
use regex::Regex;
use reqwest::Client;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;

pub struct GScrape {
    pub link: String,
    pub log: Option<PathBuf>,
}

impl GScrape {
    pub fn new(link: String, log: Option<PathBuf>) -> Self {
        Self { link, log }
    }

    pub fn parse_id(link: &str) -> Option<String> {
        let re = Regex::new(r"/file/d/([a-zA-Z0-9_-]+)").unwrap();
        if let Some(c) = re.captures(link) {
            return Some(c[1].to_string());
        }
        let re = Regex::new(r"[?&]id=([a-zA-Z0-9_-]+)").unwrap();
        if let Some(c) = re.captures(link) {
            return Some(c[1].to_string());
        }
        None
    }

    fn parse_uuid(html: &str) -> Option<String> {
        let re = Regex::new(r#"name="uuid"\s+value="([0-9a-fA-F-]+)""#).unwrap();
        re.captures(html).map(|c| c[1].to_string())
    }

    fn confirm_url(id: &str) -> String {
        format!("https://drive.usercontent.google.com/download?id={id}&export=download")
    }

    fn download_url(id: &str, uuid: &str) -> String {
        format!("https://drive.usercontent.google.com/download?id={id}&export=download&confirm=t&uuid={uuid}")
    }

    pub async fn send(&self, path: String, proto: &Protocol, neg: &str) -> bool {
        self.download(&path, proto, neg).await.is_ok()
    }

    pub async fn download(
        &self,
        path: &str,
        proto: &Protocol,
        neg: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut handle: Option<LoggingHandle> = match self.log {
            Some(ref pb) => Some(LoggingHandle::get_handle(pb).await.unwrap()),
            None => None,
        };

        let id = match Self::parse_id(&self.link) {
            Some(i) => {
                log!(handle, &format!("Parsed id: {i}\n"));
                i
            }
            None => {
                log!(handle, "Failed to parse id from link\n");
                return Err("failed to parse id".into());
            }
        };

        let client = Client::builder()
            .cookie_store(true)
            .timeout(Duration::from_secs(360))
            .build()?;

        let confirm = Self::confirm_url(&id);
        log!(handle, &format!("Confirm GET: {confirm}\n"));
        let resp = client.get(&confirm).send().await?;
        if !resp.status().is_success() {
            log!(handle, "Confirmation request failed\n");
            return Err("confirmation request failed".into());
        }

        let body = resp.text().await?;
        let uuid = match Self::parse_uuid(&body) {
            Some(u) => {
                log!(handle, &format!("Parsed uuid: {u}\n"));
                u
            }
            None => {
                log!(handle, "Failed to parse uuid from response\n");
                return Err("failed to parse uuid".into());
            }
        };

        let download = Self::download_url(&id, &uuid);
        log!(handle, &format!("Download GET: {download}\n"));

        let mut resp = client.get(&download).send().await?;
        if !resp.status().is_success() {
            log!(handle, "Download request failed\n");
            return Err("download failed".into());
        }

        let total = resp.content_length().unwrap_or(1) as f64;
        let mut downloaded: f64 = 0.0;
        let mut last_printed: f64 = -1.0;
        let mut file = File::create(Path::new(path)).await?;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk).await?;
            let n = chunk.len() as f64;
            downloaded += n;
            let percent = (downloaded / total * 100.0).ceil();
            if (percent - last_printed).abs() >= 0.1 {
                println!("{}", lib_pn_emit!(
                    protocol = proto,
                    negkey = &neg,
                    schema = [leaf, [leaf, leaf, leaf]],
                    data = ["0", [percent, downloaded, total]]
                ).unwrap());
                last_printed = percent;
            }
        }
        file.flush().await?;

        if let Some(mut h) = handle {
            h.flush().await;
        }

        Ok(())
    }
}
