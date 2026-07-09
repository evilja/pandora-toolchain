use super::*;
use pandora_toolchain::libkagami::core::{cached_normalized_font_names, collect_font_files};
use std::collections::BTreeSet;

pub async fn handle_fontcheck(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/fontcheck").await {
        Some(id) => id,
        None => return,
    };

    let mut response_msg = match working_response(ctx, command, "Counting fonts...").await {
        Some(m) => m,
        None => return,
    };

    let global_dir = PathBuf::from("DB").join("fontconfig").join("global");
    let server_dir = PathBuf::from("DB").join("fontconfig").join(server_id.to_string());

    let counts = tokio::task::spawn_blocking(move || {
        (count_fonts(&global_dir), count_fonts(&server_dir))
    }).await;
    let (global, server) = match counts {
        Ok(counts) => counts,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Font check failed: {}", e))).await;
            return;
        }
    };

    let mut all_names: BTreeSet<String> = BTreeSet::new();
    all_names.extend(global.names.iter().cloned());
    all_names.extend(server.names.iter().cloned());

    let body = format!(
        "**Font check**\nGlobal (`DB/fontconfig/global`): {} file(s), {} unique name(s)\nServer (`DB/fontconfig/{}`): {} file(s), {} unique name(s)\nTotal unique usable fonts: {}",
        global.files,
        global.names.len(),
        server_id,
        server.files,
        server.names.len(),
        all_names.len()
    );
    let _ = response_msg.edit(ctx, EditMessage::new().content(body)).await;
}

struct FontCount {
    files: usize,
    names: BTreeSet<String>,
}

fn count_fonts(dir: &Path) -> FontCount {
    let mut files: Vec<PathBuf> = Vec::new();
    collect_font_files(dir, &mut files);
    let mut names: BTreeSet<String> = BTreeSet::new();
    for path in &files {
        for name in cached_normalized_font_names(path) {
            names.insert(name);
        }
    }
    FontCount { files: files.len(), names }
}
