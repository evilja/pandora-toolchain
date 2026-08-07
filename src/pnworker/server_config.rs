use std::path::PathBuf;

// Positional index of the server-level Drive-only upload policy in `meta.pandora`.
pub const SERVER_DRIVE_ONLY_LINE: usize = 14;

// Missing and explicitly disabled values preserve the legacy multi-provider behavior. Unknown
// non-empty values fail closed to Drive-only so a damaged restrictive policy cannot silently
// re-enable public streaming-host uploads.
pub fn drive_only_from_meta(meta: &str) -> bool {
    let Some(value) = meta.lines().nth(SERVER_DRIVE_ONLY_LINE).map(str::trim) else {
        return false;
    };
    match value.to_ascii_lowercase().as_str() {
        "" | "false" | "0" | "disabled" | "off" => false,
        "true" | "1" | "enabled" | "on" => true,
        _ => true,
    }
}

pub async fn server_drive_only(server_id: Option<u64>) -> bool {
    let Some(server_id) = server_id else {
        return false;
    };
    let path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("meta.pandora");
    tokio::fs::read_to_string(path)
        .await
        .map(|meta| drive_only_from_meta(&meta))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with_policy(value: Option<&str>) -> String {
        let mut lines = vec![String::new(); SERVER_DRIVE_ONLY_LINE];
        if let Some(value) = value {
            lines.push(value.to_string());
        }
        lines.join("\n")
    }

    #[test]
    fn missing_or_disabled_policy_preserves_multi_provider_uploads() {
        assert!(!drive_only_from_meta(&meta_with_policy(None)));
        for value in ["", "false", "0", "disabled", "OFF"] {
            assert!(!drive_only_from_meta(&meta_with_policy(Some(value))));
        }
    }

    #[test]
    fn enabled_policy_is_drive_only() {
        for value in ["true", "1", "enabled", "ON"] {
            assert!(drive_only_from_meta(&meta_with_policy(Some(value))));
        }
    }

    #[test]
    fn malformed_restrictive_policy_fails_closed() {
        assert!(drive_only_from_meta(&meta_with_policy(Some("tru"))));
    }
}
