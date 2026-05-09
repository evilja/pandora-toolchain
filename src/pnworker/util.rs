use std::collections::HashMap;
use std::process::Stdio;
use tokio::io::{BufReader, AsyncBufReadExt};
use crate::libpnprotocol::core::{Protocol, TypeC};
use tokio::process::Command;
use serde::Deserialize;

#[derive(Debug)]
pub enum CliParam {
    Literal(&'static str),
    JobId(&'static str),
    Path(&'static str),
    Flag(&'static str),
    NegVer(&'static str),
    RepeatedPath(&'static str),
}

pub enum ToolResult {
    Success,
    Fail,
    Cancel,
}

pub enum PathValue {
    Single(String),
    Multi(Vec<String>),
}

impl From<String> for PathValue {
    fn from(s: String) -> Self { PathValue::Single(s) }
}

impl From<Vec<String>> for PathValue {
    fn from(v: Vec<String>) -> Self { PathValue::Multi(v) }
}

pub async fn run_tool<F>(
    tool_path: &str,
    params: &[CliParam],
    paths: &HashMap<&str, PathValue>,
    job_id: u64,
    proto: &mut Protocol,
    mut on_line: F,
) -> ToolResult
where
    F: FnMut(&TypeC) -> Option<ToolResult>,
{
    let mut cmd = Command::new(tool_path);
    for param in params {
        match param {
            CliParam::Literal(s)    => { cmd.arg(s); }
            CliParam::Flag(s)       => { cmd.arg(format!("--{}", s)); }
            CliParam::JobId(prefix) => { cmd.arg(format!("{}{}", prefix, job_id)); }
            CliParam::NegVer(v)     => { cmd.arg(v); }
            CliParam::Path(key)     => {
                if let Some(PathValue::Single(s)) = paths.get(key) {
                    cmd.arg(s);
                } else {
                    panic!("Missing or wrong type for path key: {}", key);
                }
            }
            CliParam::RepeatedPath(key) => {
                if let Some(PathValue::Multi(values)) = paths.get(key) {
                    for v in values {
                        cmd.arg("--candidate");
                        cmd.arg(v);
                    }
                }
            }
        }
    }
    cmd.stderr(Stdio::null());
    cmd.stdout(Stdio::piped());
    let mut child = cmd.spawn().expect("Failed to spawn tool");
    let stdout = child.stdout.take().expect("No stdout");
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();
    let mut negotiated = false;

    while let Ok(Some(line)) = lines.next_line().await {
        println!("{}", line);
        if !negotiated {
            if proto.negotiate(&line).is_ok() { negotiated = true; }
        } else if let Ok(data) = proto.extract_data(&line) {
            if let Some(result) = on_line(&data) {
                child.kill().await.ok();
                return result;
            }
        }
    }

    match child.wait().await.expect("Failed to wait on child").success() {
        true => ToolResult::Success,
        false => ToolResult::Fail,
    }
}

#[derive(Deserialize, Debug)]
pub struct IntrosConfig {
    pub groups: HashMap<String, Vec<String>>,
}

impl IntrosConfig {
    pub fn load() -> Self {
        std::fs::read_to_string("intros.toml")
            .ok()
            .and_then(|c| toml::from_str(&c).ok())
            .unwrap_or(IntrosConfig { groups: HashMap::new() })
    }

    pub fn resolve(&self, group: &str) -> Option<Vec<String>> {
        self.groups.get(group).cloned()
    }
}

#[inline]
pub fn string_byte_to_mb(s: &str) -> u16 {
    (s.parse::<u64>().unwrap_or(1) / 1024 / 1024) as u16
}
