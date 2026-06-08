use std::{collections::HashMap, fs::{File, OpenOptions}, io::{Read, Write}};

use crate::libpnenv::standard::{ENV_PATH, ENV_SEP};



pub fn get_env(envfile: &str) -> HashMap<String, String> {
    let mut file = match File::open(&envfile) {
        Ok(f) => f,
        Err(_) => return HashMap::new(),
    };
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return HashMap::new();
    }
    let mut map = HashMap::new();
    for line in buf.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(ENV_SEP) {
            map.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    map
}

pub fn get_pandora_env() -> HashMap<String, String> {
    get_env(ENV_PATH)
}

pub fn add_env(envfile: &str, string: &mut String) -> bool {
    let mut file = match OpenOptions::new().write(true).append(true).open(envfile) {
        Ok(a) => a,
        Err(_) => {
            return false
        }
    };
    string.push('\n');
    match file.write(&string.as_bytes()) {
        Ok(_) => {return true;}
        Err(_) => {return false;}
    }
}
pub fn upsert_env(envfile: &str, key: &str, value: &str) -> Result<bool, String> {
    let mut lines: Vec<String> = match std::fs::read_to_string(envfile) {
        Ok(contents) => contents.lines().map(|l| l.to_string()).collect(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e.to_string()),
    };
    let target = format!("{}{}{}", key, ENV_SEP, value);
    let mut replaced = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((existing_key, _)) = trimmed.split_once(ENV_SEP) {
            if existing_key.trim() == key {
                *line = target.clone();
                replaced = true;
                break;
            }
        }
    }
    if !replaced {
        lines.push(target);
    }
    let mut out = String::new();
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(envfile, out).map_err(|e| e.to_string())?;
    Ok(replaced)
}
pub fn remove_env(envfile: &str, target: &str) -> Result<bool, String> {
    let contents = std::fs::read_to_string(envfile).map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = contents.lines().map(|l| l.to_string()).collect();
    let original_len = lines.len();
    lines.retain(|l| l.trim() != target);
    if lines.len() == original_len {
        return Ok(false);
    }
    let mut out = String::new();
    for line in &lines {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(envfile, out).map_err(|e| e.to_string())?;
    Ok(true)
}
pub fn get_perm(envfile: String) -> Vec<String> {
    // Q: CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, TOKEN_URL, TOKEN, UPLOAD_URL
    let mut file = match File::open(&envfile) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let mut buf = String::new();
    if file.read_to_string(&mut buf).is_err() {
        return vec![];
    }
    let lines: Vec<String> = buf.lines().map(|line| line.to_string()).collect();
    lines.clone()
}
