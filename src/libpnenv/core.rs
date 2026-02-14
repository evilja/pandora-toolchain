use std::{fs::File, io::Read};

pub fn get_env(envfile: String) -> Vec<String> {
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
    if lines.len() < 10 {
        eprintln!("Warning: env.pandora has only {} lines, expected 10", lines.len());
        return vec![];
    }
    lines.clone()
}
