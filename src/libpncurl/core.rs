use crate::libpnenv::{
    core::get_env,
    standard::{
        CLIENT_ID,
        CLIENT_SECRET,
        REFRESH_TOKEN,
        TOKEN_URL,
    }
};
use reqwest::blocking::{Client, multipart};
use serde::Deserialize;
use std::fs::File;
use std::io::Read;
use std::io::{self, Write};
use std::path::Path;
use std::sync::mpsc::Sender;

use std::io::Result as ioResult;

struct ProgressReader<R: Read> {
    inner: R,
    sent: u64,
    total: u64,
    tx: Sender<RpbData>,
    last_progress: u16,
}

impl<R: Read> ProgressReader<R> {
    fn new(inner: R, total: u64, tx: Sender<RpbData>) -> Self {
        Self {
            inner,
            sent: 0,
            total,
            tx,
            last_progress: 0,
        }
    }
}

impl<R: Read> Read for ProgressReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> ioResult<usize> {
        let n = self.inner.read(buf)?;

        if n > 0 {
            self.sent += n as u64;

            self.tx.send(RpbData::Progress(self.sent, self.total)).ok();
        }

        Ok(n)
    }
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
}

fn get_access_token(
    // CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, TOKEN_URL
    env: Vec<String>,
) -> Result<String, Box<dyn std::error::Error>> {
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
        .send()?
        .error_for_status()?;

    let token: TokenResponse = resp.json()?;

    Ok(token.access_token)
}

pub enum RpbData {
    Progress(u64, u64),
    Done(String),
    Fail,
}

pub struct Req {
    pub target: String,
}

impl Req {
    pub fn send(&self, path: String) -> bool {
        match self.download(&path) {
            Ok(_) => true,
            Err(_) => false,
        }
    }

    fn download(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let response = reqwest::blocking::get(&self.target)?;

        if !response.status().is_success() {
            return Err("Request failed".into());
        }

        let bytes = response.bytes()?;

        let mut file = File::create(Path::new(path))?;
        file.write_all(&bytes)?;

        Ok(())
    }

    pub fn gdupload(
        &self,
        envpath: String,
        outfile: Option<String>,
        parent_id: &str,
        tx: Sender<RpbData>,
    ) -> bool {
        let access_token = match get_access_token(get_env(envpath)) {
            Ok(token) => token,
            Err(_) => return false,
        };

        let client = Client::new();

        let upload_name = outfile.clone().unwrap_or(self.target.clone());

        let metadata = serde_json::json!({
            "name": upload_name,
            "parents": [parent_id],
        });

        let file = match File::open(&self.target) {
            Ok(f) => f,
            Err(_) => return false,
        };

        let total_size = file.metadata().unwrap().len();

        let upload_name = outfile.clone().unwrap_or(self.target.clone());

        let reader = ProgressReader::new(file, total_size, tx.clone());

        let part = multipart::Part::reader(reader)
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
            .send()
        {
            Ok(r) => r,
            Err(a) => {
                println!("{:?}", a);
                tx.send(RpbData::Fail).ok();
                return false;
            }
        };

        let json: serde_json::Value = match resp.json() {
            Ok(j) => j,
            Err(a) => {
                println!("{:?}", a);
                tx.send(RpbData::Fail).ok();
                return false;
            }
        };

        let file_id = match json["id"].as_str() {
            Some(id) => id,
            None => {
                tx.send(RpbData::Fail).ok();
                return false;
            }
        };

        let link = format!(
            "https://drive.google.com/file/d/{}/view?usp=sharing",
            file_id
        );

        tx.send(RpbData::Done(link.clone())).ok();

        true
    }
}
