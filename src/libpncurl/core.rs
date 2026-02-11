
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

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
}

