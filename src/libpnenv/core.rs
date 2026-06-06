use std::{fs::{File, OpenOptions}, io::{Read, Write}};



pub fn get_env(envfile: &str) -> Vec<String> {
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
    if lines.len() < 16 {
        eprintln!("Warning: env.pandora has only {} lines, expected 16", lines.len());
        return vec![];
    }
    lines.clone()
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
pub fn remove_env(envfile: &str, target: &str) -> Result<bool, String> {
    let contents = std::fs::read_to_string(envfile).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = contents.lines().collect();
    let original_len = lines.len();
    let filtered: Vec<&str> = lines.into_iter().filter(|l| l.trim() != target).collect();
    if filtered.len() == original_len {
        return Ok(false);
    }
    let mut out = String::new();
    for line in filtered {
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
