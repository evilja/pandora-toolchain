use std::path::Path;

use crate::lib::env::{core::get_pandora_env, standard::LINK_NODE_NAME};

pub const IDENTIFIER_PREFIX: &str = "SubstitutePandoraIdentifier";
pub const DEFAULT_LABEL: &str = "Evillja Cloud Solutions";
pub const SIGNATURE_PATH: &str = "DB/config/global/environment/signature.pandora";
pub const NODE_LABEL_ENV: &str = "PANDORA_VIDEO_NODE_LABEL";
pub const MAX_LABEL_CHARS: usize = 128;

pub fn valid_label(label: &str) -> bool {
    if label.chars().any(char::is_control) { return false; }
    let label = label.trim();
    !label.is_empty()
        && label.chars().count() <= MAX_LABEL_CHARS
}

pub fn identifier_for_label(label: &str) -> String {
    format!("{IDENTIFIER_PREFIX} - {}", label.trim())
}

// Existing node identities have no label-length limit; the command's 128-character limit only
// applies to a main-node label. Do not silently credit the main node for a longer Mini name.
fn valid_node_label(label: &str) -> bool {
    !label.trim().is_empty() && !label.chars().any(char::is_control)
}

pub fn is_identifier(value: &str) -> bool {
    value.strip_prefix(IDENTIFIER_PREFIX).and_then(|suffix| suffix.strip_prefix(" - "))
        .is_some_and(valid_node_label)
}

// Mini's identity wins over any coordinator config copied onto its disk. The environment value
// carries that identity into pnmpeg, which does not inherit pndc's --mini argument.
pub fn mini_label() -> Option<String> {
    if !crate::pnworker::link::client::is_mini() {
        return None;
    }
    get_pandora_env().get(LINK_NODE_NAME).cloned()
        .filter(|label| valid_node_label(label))
}

fn resolve_label<'a>(node: Option<&'a str>, configured: Option<&'a str>) -> &'a str {
    node.filter(|label| valid_node_label(label))
        .or_else(|| configured.map(str::trim).filter(|label| valid_label(label)))
        .unwrap_or(DEFAULT_LABEL)
        .trim()
}

// Read at each output, so /signature takes effect without restarting either pndc or its tools.
pub fn video_identifier() -> String {
    let node = std::env::var(NODE_LABEL_ENV).ok().or_else(mini_label);
    let configured = std::fs::read_to_string(SIGNATURE_PATH).ok();
    identifier_for_label(resolve_label(node.as_deref(), configured.as_deref()))
}

// A unique sibling and rename keep concurrent readers from observing an empty or partial label.
pub async fn write_label(label: &str) -> Result<(), String> {
    if !valid_label(label) {
        return Err("invalid signature label".into());
    }
    let path = Path::new(SIGNATURE_PATH);
    let parent = path.parent().unwrap();
    tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    let token = crate::lib::secret::random_hex_token().map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".signature-{token}.tmp"));
    let result = async {
        tokio::fs::write(&temporary, label.trim()).await?;
        tokio::fs::rename(&temporary, path).await
    }.await;
    if result.is_err() {
        tokio::fs::remove_file(&temporary).await.ok();
    }
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mini_identity_overrides_a_copied_main_signature() {
        assert_eq!(resolve_label(Some("mini-hoshi"), Some("Main")), "mini-hoshi");
        assert_eq!(resolve_label(None, Some(" Main ")), "Main");
        assert_eq!(resolve_label(None, Some("Main\n")), "Main");
        let long_node = "n".repeat(MAX_LABEL_CHARS + 1);
        assert_eq!(resolve_label(Some(&long_node), Some("Main")), long_node);
        assert_eq!(resolve_label(None, None), DEFAULT_LABEL);
        assert_eq!(resolve_label(None, Some("\n")), DEFAULT_LABEL);
        assert_eq!(identifier_for_label("mini-hoshi"), "SubstitutePandoraIdentifier - mini-hoshi");
    }

    #[test]
    fn labels_are_single_line_bounded_unicode_text() {
        assert!(valid_label("幸せの魔女 - Main"));
        assert!(valid_label(&"あ".repeat(MAX_LABEL_CHARS)));
        assert!(!valid_label(&"あ".repeat(MAX_LABEL_CHARS + 1)));
        assert!(!valid_label("a\nb"));
        assert!(!valid_label("a\n"));
        assert!(!valid_label("a\0b"));
        assert!(!valid_label("   "));
    }
}
