// Named credential values a profile refers to as `${secret.name}`. They live in their own file so a
// profile can be read, diffed and pasted into a bug report without carrying an API key with it —
// and so the file holding the keys can be given the same restricted permissions as the token file.
//
// A profile may still write a literal header value instead. Both are private deployment
// configuration; this is the form that keeps the credential out of the part an operator edits most.

use std::collections::BTreeMap;

use crate::lib::env::standard::LINK_BOOT_SECRETS_PATH;

// Read on every attempt rather than cached. Boots are rare, the file is small, and a cached copy
// would mean a rotated key does not take effect until the process restarts — which is the sort of
// thing that is discovered during the incident the rotation was for.
pub fn load() -> Result<BTreeMap<String, String>, String> {
    let contents = match std::fs::read_to_string(LINK_BOOT_SECRETS_PATH) {
        Ok(contents) => contents,
        // No file is not an error: a profile that references no secret needs none, and saying so
        // here would make every such deployment look misconfigured.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("could not read {LINK_BOOT_SECRETS_PATH}: {e}")),
    };
    let parsed: toml::Value = toml::from_str(&contents)
        .map_err(|e| format!("could not parse {LINK_BOOT_SECRETS_PATH}: {e}"))?;
    let table = parsed
        .as_table()
        .ok_or_else(|| format!("{LINK_BOOT_SECRETS_PATH} must be a table of name = \"value\""))?;
    let mut out = BTreeMap::new();
    for (name, value) in table {
        let text = match value {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Boolean(b) => b.to_string(),
            _ => {
                return Err(format!(
                    "{LINK_BOOT_SECRETS_PATH}: `{name}` must be a string"
                ));
            }
        };
        out.insert(name.clone(), text);
    }
    Ok(out)
}

// The names only. `/lsnode` and the profile validator need to say whether a reference resolves
// without ever holding the value, and a list of names is the whole of what they need.
pub fn names() -> Vec<String> {
    load().map(|m| m.into_keys().collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_missing_secrets_file_is_an_empty_map_and_not_an_error() {
        // The path is a fixed relative one; under `cargo test` the working directory has no `DB/`,
        // which is exactly the "profile references no secret" case this must not fail.
        assert!(super::load().is_ok());
    }
}
