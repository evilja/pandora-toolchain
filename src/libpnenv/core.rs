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
    vec![lines[0].clone(), lines[1].clone(), lines[2].clone(),
        lines[3].clone(), lines[4].clone(), lines[5].clone()]
}
