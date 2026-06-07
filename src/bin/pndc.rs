use serenity::{
    Client,
    all::{ActivityData, ChannelType, CommandOptionType, Context, CreateEmbed, CreateMessage, EditMessage, GatewayIntents, Interaction, Message, OnlineStatus, Ready},
    builder::{CreateCommand, CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage, EditChannel},
    prelude::*,
};
use pandora_toolchain::libpnp2p::nyaaise::nyaaise;
use pandora_toolchain::pnworker::core::{HalfJob, Job, JobClass, JobType, Preset};
use pandora_toolchain::pnworker::util::{CliParam, IntrosConfig, PathValue, ToolResult, run_tool};
use pandora_toolchain::pnworker::tools::PNASS_LAYER;
use pandora_toolchain::pnworker::tools::PNASS_MERGE;
use pandora_toolchain::pnworker::tools::PNASS_MERGE_TL_ONLY;
use pandora_toolchain::libpnenv::{
    core::{add_env, get_pandora_env, get_perm, remove_env},
    standard::TOKEN,
};
use pandora_toolchain::libpnmal::{fetch_anime, AnimeMeta, AnimeKind};
use pandora_toolchain::libpnforgejo::{Forgejo, base64_encode, base64_encode_bytes};
use pandora_toolchain::pnworker::core::pn_worker;
use pandora_toolchain::libpnenv::standard::PNASS;
use pandora_toolchain::libkagami::core::SubstationAlpha;
use pandora_toolchain::libpnprotocol::core::Protocol;
use tokio::sync::mpsc::{channel, Sender, Receiver};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use regex::Regex;
use reqwest;

pub struct Handler {
    pub tx: Sender<JobClass>,
    pub intros: IntrosConfig,
}

const ALL_LEVELS: &[&str] = &[
    "upper.pandora",
    "admin.pandora",
    "fansubber.pandora",
    "authorize.pandora",
];

fn level_rank(name: &str) -> u8 {
    match name {
        "upper.pandora" => 3,
        "admin.pandora" => 2,
        "fansubber.pandora" => 1,
        "authorize.pandora" => 0,
        _ => u8::MAX,
    }
}

fn has_level_at_least(id: u64, min_rank: u8) -> bool {
    ALL_LEVELS.iter().any(|lvl| {
        if level_rank(lvl) >= min_rank {
            let allowed = get_perm(perm_path(lvl));
            !allowed.is_empty() && allowed.contains(&id.to_string())
        } else {
            false
        }
    })
}

fn min_rank_for_command(part: &str) -> u8 {
    match part {
        "encode" | "pancode" | "probe" | "backup" | "scrape" | "gitcode" | "smartcode" | "source" => 0,
        "!enc" | "!encode" => 0,
        "job" => 1,
        "auth" | "remove" | "gitsync" | "hearts" | "configure" | "readmebase" | "!ban" | "!some" => 2,
        "attach" | "init" | "destruct" | "detach" => 3,
        _ => u8::MAX,
    }
}

fn is_authorized(part: &str, id: u64) -> bool {
    let min_rank = min_rank_for_command(part);
    if min_rank == u8::MAX { return false; }
    has_level_at_least(id, min_rank)
}

fn read_lang(guild_id: Option<serenity::all::GuildId>) -> String {
    let id = match guild_id {
        Some(g) => g.get(),
        None => return "tr".to_string(),
    };
    let path = format!("DB/config/{}/meta.pandora", id);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.lines().next().map(String::from))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tr".to_string())
}

pub async fn handle_message(
    context: &Context,
    msg: &Message,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    if msg.attachments.is_empty() {
        msg.reply(context, "Error: Subtitle attachment required").await.ok();
        return None;
    }

    let attachment_bytes = match msg.attachments[0].download().await {
        Ok(b) => b,
        Err(e) => {
            msg.reply(context, format!("Failed to download attachment: {}", e)).await.ok();
            return None;
        }
    };

    let response_msg = match msg.channel_id.send_message(context, CreateMessage::new().content("...")).await {
        Ok(m) => m,
        Err(e) => {
            msg.reply(context, format!("Failed to send response: {}", e)).await.ok();
            return None;
        }
    };

    response_msg.react(context, '❌').await.ok();

    Some(Job::new(
        msg.author.id.get(),
        msg.channel_id.get(),
        response_msg.id.get(),
        JobType::Encode,
        msg.id.get(),
        preset,
        nyaaise(&torrent_url),
        attachment_bytes,
        context.clone(),
        response_msg,
        read_lang(msg.guild_id),
    ))
}

pub async fn handle_probe(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Probing torrent...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Probe,
        response_msg.id.get(),
        Preset::Dummy(None),   // irrelevant for probe
        nyaaise(&torrent_url),
        vec![],                // no attachment
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
    ))
}

pub async fn handle_backup(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Backup process will begin shortly after...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Backup,
        response_msg.id.get(),
        Preset::Dummy(None),
        nyaaise(&torrent_url),
        vec![],
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
    ))
}

pub async fn handle_scrape(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Scraping...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Backup,
        response_msg.id.get(),
        Preset::Dummy(None),
        nyaaise(&torrent_url),
        vec![],
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
    ))
}

fn pad2(n: u32) -> String {
    if n < 100 {
        format!("{:02}", n)
    } else {
        n.to_string()
    }
}

fn parse_repo_url(url: &str) -> Result<(String, String), String> {
    let re = regex::Regex::new(r"^https?://[^/]+/([^/]+)/([^/]+)/?$").unwrap();
    let caps = re.captures(url.trim_end_matches('/'))
        .ok_or_else(|| format!("not a Forgejo repo URL: {}", url))?;
    let owner = caps.get(1).unwrap().as_str().to_string();
    let repo = caps.get(2).unwrap().as_str().to_string();
    Ok((owner, repo))
}

#[derive(serde::Deserialize, Default)]
struct ChannelMeta {
    mal_id: Option<u64>,
    kind: Option<String>,
    name: Option<String>,
    slug: Option<String>,
    episode_count: Option<u32>,
    repo_url: Option<String>,
    episode_count_at_git: Option<u32>,
    year: Option<u16>,
    #[serde(default = "default_season")]
    season: u16,
    #[serde(default = "default_credit")]
    tl: String,
    #[serde(default = "default_credit")]
    tlc: String,
    #[serde(default = "default_credit")]
    ts: String,
    #[serde(default = "default_credit")]
    qc: String,
}

fn default_season() -> u16 { 1 }

fn default_credit() -> String { "---".to_string() }

fn perm_path(name: &str) -> String {
    format!("DB/config/global/perms/{}", name)
}

async fn migrate_pandora_files() {
    let _ = tokio::fs::create_dir_all("DB/config/global/perms").await;
    let _ = tokio::fs::create_dir_all("DB/config/global/environment").await;

    for name in ["authorize.pandora", "upper.pandora", "fansubber.pandora", "admin.pandora"] {
        let old = name.to_string();
        let new_path = format!("DB/config/global/perms/{}", name);
        if std::path::Path::new(&old).exists() && !std::path::Path::new(&new_path).exists() {
            match std::fs::rename(&old, &new_path) {
                Ok(()) => println!("Migrated {} -> {}", old, new_path),
                Err(e) => eprintln!("Warning: failed to migrate {} -> {}: {}", old, new_path, e),
            }
        }
    }

    let env_old = "env.pandora";
    let env_new = "DB/config/global/environment/env.pandora";
    if std::path::Path::new(env_old).exists() && !std::path::Path::new(env_new).exists() {
        match std::fs::rename(env_old, env_new) {
            Ok(()) => println!("Migrated {} -> {}", env_old, env_new),
            Err(e) => eprintln!("Warning: failed to migrate {} -> {}: {}", env_old, env_new, e),
        }
    }

    migrate_env_format().await;
}

async fn migrate_env_format() {
    use pandora_toolchain::libpnenv::standard::ENV_SEP;

    let path = pandora_toolchain::libpnenv::standard::ENV_PATH;
    let contents = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => return,
    };

    let is_new = contents.lines()
        .map(str::trim)
        .any(|l| !l.is_empty() && !l.starts_with('#') && l.contains(ENV_SEP));
    if is_new {
        return;
    }

    let mapping: &[(&str, usize)] = &[
        ("gdrive_client_id", 0),
        ("gdrive_client_secret", 1),
        ("gdrive_refresh_token", 2),
        ("gdrive_token_url", 3),
        ("discord_token", 4),
        ("gdrive_upload_url", 5),
        ("pnmpeg", 6),
        ("pnp2p", 7),
        ("pncurl", 8),
        ("gdrive_parent_id", 9),
        ("doodstream", 10),
        ("uqload", 11),
        ("lulu", 12),
        ("voesx", 13),
        ("abyss", 14),
        ("pnass", 15),
    ];

    let lines: Vec<&str> = contents.lines().collect();
    let mut out = String::new();
    for (name, idx) in mapping {
        if let Some(value) = lines.get(*idx) {
            out.push_str(&format!("{}{}{}\n", name, ENV_SEP, value));
        }
    }

    match std::fs::write(path, out) {
        Ok(()) => println!("Migrated env.pandora to new format"),
        Err(e) => eprintln!("Warning: failed to migrate env.pandora to new format: {}", e),
    }
}

fn read_credit_option(command: &serenity::all::CommandInteraction, name: &str) -> String {
    command.data.options.iter()
        .find(|opt| opt.name == name)
        .and_then(|opt| opt.value.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("---")
        .to_string()
}

fn meta_to_toml(m: &ChannelMeta) -> String {
    match (&m.kind, m.mal_id) {
        (Some(k), Some(id)) => {
            let mut out = format!(
                "mal_id = {}\nkind = \"{}\"\nname = \"{}\"\nslug = \"{}\"\nepisode_count = {}\nrepo_url = \"{}\"\nseason = {}\ntl = \"{}\"\ntlc = \"{}\"\nts = \"{}\"\nqc = \"{}\"\n",
                id, k, m.name.as_deref().unwrap_or(""), m.slug.as_deref().unwrap_or(""),
                m.episode_count.unwrap_or(0), m.repo_url.as_deref().unwrap_or(""),
                m.season, m.tl, m.tlc, m.ts, m.qc
            );
            if let Some(y) = m.year {
                out.push_str(&format!("year = {}\n", y));
            }
            if let Some(c) = m.episode_count_at_git {
                out.push_str(&format!("episode_count_at_git = {}\n", c));
            }
            out
        }
        _ => String::new(),
    }
}

pub async fn handle_smartcode(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    intros: &IntrosConfig,
) -> Option<Job> {
    let episode = match command.data.options.iter()
        .find(|opt| opt.name == "episode")
        .and_then(|opt| opt.value.as_i64())
    {
        Some(n) if n >= 1 && n <= u32::MAX as i64 => n as u32,
        _ => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `episode` must be a positive integer.")
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    let link_opt = command.data.options.iter()
        .find(|opt| opt.name == "link")
        .and_then(|opt| opt.value.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from);

    let candidates = command.data.options.iter()
        .find(|opt| opt.name == "concat")
        .and_then(|opt| opt.value.as_str())
        .and_then(|group| intros.resolve(group));

    let preset = match command.data.options.iter()
        .find(|opt| opt.name == "preset")
        .and_then(|opt| opt.value.as_str())
        .unwrap_or("standard")
    {
        "gpu"             => Preset::Standard(candidates.clone()),
        "standard"        => Preset::Standard(candidates.clone()),
        "dummy"           => Preset::Dummy(candidates.clone()),
        _                 => Preset::PseudoLossless(candidates.clone()),
    };

    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: /smartcode can only be used in a server")
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };
    let channel_id = command.channel_id.get();

    let meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: this channel is not attached to an anime. Run `/init` or `/attach` first.")
                .ephemeral(true)
        )).await.ok();
        return None;
    }
    let name = meta.name.clone().unwrap_or_default();
    let max_ep = meta.episode_count.unwrap_or(0);
    if episode < 1 || episode > max_ep {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                    .content(format!("Error: `episode` must be between 1 and {}.", max_ep))
                    .ephemeral(true)
        )).await.ok();
        return None;
    }
    let repo_url = meta.repo_url.clone().unwrap_or_default();
    let (owner, repo) = match parse_repo_url(&repo_url) {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: bad repo URL in meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };
    let owner_repo = format!("{}/{}", owner, repo);

    let (_lang, forgejo_base, api_key) = match read_server_meta(server_id).await {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: failed to read server meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };
    if forgejo_base.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: server has no forgejo org configured. Run `/configure` first.")
                .ephemeral(true)
        )).await.ok();
        return None;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Working…")
    )).await.ok();
    let mut response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return None;
        }
    };

    let safe_name = name.replace('/', "-");
    let folder = pad2(episode);
    let tl_path = format!("{}/TL - {} - E{:02}.ass", folder, safe_name, episode);
    let ts_path = format!("{}/TS - {} - E{:02}.ass", folder, safe_name, episode);

    let tl_b64 = match fg.get_file_content(&owner_repo, &tl_path).await {
        Ok(Some((b, _))) => b,
        Ok(None) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("TL file not found at `{}`.", tl_path))).await;
            return None;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to fetch TL: {}", e))).await;
            return None;
        }
    };
    let tl_bytes = match base64_decode_bytes(&tl_b64) {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to decode TL base64: {}", e))).await;
            return None;
        }
    };

    let ts_b64_opt = match fg.get_file_content(&owner_repo, &ts_path).await {
        Ok(Some((b, _))) => Some(b),
        Ok(None) => None,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to fetch TS: {}", e))).await;
            return None;
        }
    };
    let ts_bytes_opt: Option<Vec<u8>> = match ts_b64_opt {
        Some(b64) => match base64_decode_bytes(&b64) {
            Ok(b) => Some(b),
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Failed to decode TS base64: {}", e))).await;
                return None;
            }
        },
        None => None,
    };

    let link = match link_opt {
        Some(ref l) => l.clone(),
        None => {
            let source_md_path = format!("{}/SOURCE.md", folder);
            let b64 = match fg.get_file_content(&owner_repo, &source_md_path).await {
                Ok(Some((b, _))) => b,
                Ok(None) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("`link` was not provided and no `{}` exists in the repo to read it from.",
                            source_md_path))).await;
                    return None;
                }
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to fetch `{}`: {}", source_md_path, e))).await;
                    return None;
                }
            };
            let bytes = match base64_decode_bytes(&b64) {
                Ok(b) => b,
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to decode `{}` base64: {}", source_md_path, e))).await;
                    return None;
                }
            };
            let text = match String::from_utf8(bytes) {
                Ok(t) => t,
                Err(e) => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("`{}` is not valid UTF-8: {}", source_md_path, e))).await;
                    return None;
                }
            };
            let parsed = text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with(';'))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .filter(|s| !s.is_empty());
            match parsed {
                Some(p) => p,
                None => {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("`{}` does not contain a parseable source link.", source_md_path))).await;
                    return None;
                }
            }
        }
    };

    let pnass_path = match get_pandora_env().get(PNASS) {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content("Error: PNASS binary path is not set in DB/config/global/environment/env.pandora.")).await;
            return None;
        }
    };

    let job_id = response_msg.id.get();
    let work_dir = match std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
    {
        Ok(d) => std::env::temp_dir().join(format!("pandora_smartcode_{}", d.as_nanos())),
        Err(_) => std::env::temp_dir().join(format!("pandora_smartcode_{}", job_id)),
    };
    if let Err(e) = tokio::fs::create_dir_all(&work_dir).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to create work dir: {}", e))).await;
        return None;
    }

    let tl_local = work_dir.join("tl.ass");
    let ts_local = work_dir.join("ts.ass");
    let merged_local = work_dir.join("merged.ass");

    if let Err(e) = tokio::fs::write(&tl_local, &tl_bytes).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to write TL: {}", e))).await;
        return None;
    }
    if let Some(ref b) = ts_bytes_opt {
        if let Err(e) = tokio::fs::write(&ts_local, b).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write TS: {}", e))).await;
            return None;
        }
    }

    let spec: &[CliParam] = if ts_bytes_opt.is_some() { PNASS_MERGE } else { PNASS_MERGE_TL_ONLY };
    let mut paths: HashMap<&str, PathValue> = HashMap::from([
        ("INPUT",  PathValue::from(tl_local.display().to_string())),
        ("OUTPUT", PathValue::from(merged_local.display().to_string())),
    ]);
    if ts_bytes_opt.is_some() {
        paths.insert("MERGE", PathValue::from(ts_local.display().to_string()));
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut proto = Protocol::new(vec![1]);
    let result = run_tool(
        &pnass_path,
        spec,
        &paths,
        job_id,
        &mut proto,
        |data| {
            if data.get(0).and_then(|v| v.as_str()) == Some("4") {
                if let Some(line) = data.get(1).and_then(|v| v.as_str()) {
                    warnings.push(line.to_string());
                }
            }
            None
        },
    ).await;
    if !matches!(result, ToolResult::Success) {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("ASS merge failed (warnings so far: {}).", warnings.len()))).await;
        return None;
    }

    let merged_bytes = match tokio::fs::read(&merged_local).await {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to read merged ASS: {}", e))).await;
            return None;
        }
    };

    let release_path = format!("{}/Release - {} - E{:02}.ass", folder, safe_name, episode);
    let release_b64 = base64_encode_bytes(&merged_bytes);
    let release_commit = "Smartcode merge".to_string();
    match fg.upsert_file(&owner_repo, &release_path, &release_b64, &release_commit).await {
        Ok(()) => {
            println!("[smartcode] uploaded {} ({} bytes)", release_path, merged_bytes.len());
        }
        Err(e) => {
            println!("[smartcode] release upload failed for {}: {}", release_path, e);
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Merged ASS upload to `{}` failed: {}\nEncoding will continue with the local file.",
                    release_path, e))).await;
            return None;
        }
    }

    let source_path = format!("{}/SOURCE.md", folder);
    if link_opt.is_none() {
        println!("[smartcode] source from {} (skipping rewrite)", source_path);
    } else {
        let source_content = format!("# {}\n", link.trim());
        let source_b64 = base64_encode(&source_content);
        let source_commit = "Smartcode source".to_string();
        match fg.upsert_file(&owner_repo, &source_path, &source_b64, &source_commit).await {
            Ok(()) => {
                println!("[smartcode] uploaded {}", source_path);
            }
            Err(e) => {
                println!("[smartcode] SOURCE.md upload failed for {}: {}", source_path, e);
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("SOURCE.md upload to `{}` failed: {}\nEncoding will continue with the local file.",
                        source_path, e))).await;
            return None;
        }
    }
    }

    println!("[smartcode] repo={} episode={} tl={} ts_presence={} warnings={} merged_bytes={} release={} source_origin={}",
        owner_repo, episode, tl_path,
        if ts_bytes_opt.is_some() { "present" } else { "absent" },
        warnings.len(), merged_bytes.len(), release_path,
        if link_opt.is_some() { "argument" } else { "SOURCE.md" });

    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let _ = response_msg.edit(ctx, EditMessage::new().content("...")).await;

    response_msg.react(ctx, '❌').await.ok();

    let final_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        final_msg.id.get(),
        JobType::Encode,
        final_msg.id.get(),
        preset,
        nyaaise(&link),
        merged_bytes,
        ctx.clone(),
        final_msg,
        read_lang(command.guild_id),
    ))
}

pub async fn handle_source(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let episode = match command.data.options.iter()
        .find(|opt| opt.name == "episode")
        .and_then(|opt| opt.value.as_i64())
    {
        Some(n) if n >= 1 && n <= u32::MAX as i64 => n as u32,
        _ => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `episode` must be a positive integer.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let link = match command.data.options.iter()
        .find(|opt| opt.name == "link")
        .and_then(|opt| opt.value.as_str())
    {
        Some(s) => s.trim().to_string(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `link` is required.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    if link.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: `link` must not be empty.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: /source can only be used in a server")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let channel_id = command.channel_id.get();

    let meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: this channel is not attached to an anime. Run `/init` or `/attach` first.")
                .ephemeral(true)
        )).await.ok();
        return;
    }
    let max_ep = meta.episode_count.unwrap_or(0);
    if episode < 1 || episode > max_ep {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: `episode` must be between 1 and {}.", max_ep))
                .ephemeral(true)
        )).await.ok();
        return;
    }
    let repo_url = match meta.repo_url.clone().filter(|s| !s.is_empty()) {
        Some(u) => u,
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: this channel has no repo URL configured.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let (owner, repo) = match parse_repo_url(&repo_url) {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: bad repo URL in meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let owner_repo = format!("{}/{}", owner, repo);

    let (_lang, forgejo_base, api_key) = match read_server_meta(server_id).await {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: failed to read server meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    if forgejo_base.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: server has no forgejo org configured. Run `/configure` first.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Working…")
    )).await.ok();
    let mut response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return,
    };

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    let folder = pad2(episode);
    let source_path = format!("{}/SOURCE.md", folder);
    let source_content = format!("# {}\n", link);
    let source_b64 = base64_encode(&source_content);
    match fg.upsert_file(&owner_repo, &source_path, &source_b64, "Set source link").await {
        Ok(()) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Wrote `{}` with:\n```\n{}\n```", source_path, source_content.trim_end()))).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write `{}`: {}", source_path, e))).await;
        }
    }
}

fn meta_path(server_id: u64, channel_id: u64) -> std::path::PathBuf {
    std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join(channel_id.to_string())
        .join("meta.toml")
}

fn read_channel_meta(server_id: u64, channel_id: u64) -> ChannelMeta {
    let path = meta_path(server_id, channel_id);
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => ChannelMeta::default(),
    }
}

async fn write_channel_meta(server_id: u64, channel_id: u64, m: &ChannelMeta) -> Result<(), String> {
    let path = meta_path(server_id, channel_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&path, meta_to_toml(m)).await.map_err(|e| e.to_string())?;
    Ok(())
}

async fn bootstrap_repo(
    fg: &Forgejo,
    owner_repo: &str,
    meta: &AnimeMeta,
    base_md: Option<String>,
    existing: Vec<String>,
) -> Result<Vec<String>, String> {
    let mut created: Vec<String> = Vec::new();

    let existing_nums: Vec<u32> = existing.iter()
        .filter_map(|n| n.trim_start_matches('0').parse::<u32>().ok().filter(|v| *v > 0).or_else(|| {
            if n == "0" { Some(0) } else { None }
        }))
        .collect();

    let empty_b64 = base64_encode("");

    for n in 1..=meta.episode_count {
        if existing_nums.contains(&n) { continue; }
        let folder = pad2(n);
        let path = format!("{}/.gitkeep", folder);
        fg.create_file(owner_repo, &path, &empty_b64, "bootstrap episode folder").await?;
        created.push(folder);
    }

    let has_readme = existing.iter().any(|n| n.eq_ignore_ascii_case("README.md"));
    if let Some(readme) = base_md {
        let b64 = base64_encode(&readme);
        if has_readme {
            let sha = fg.get_file_sha(owner_repo, "README.md").await?
                .ok_or_else(|| "README.md disappeared between list and update".to_string())?;
            fg.update_file(owner_repo, "README.md", &b64, &sha, "bootstrap root readme").await?;
        } else {
            fg.create_file(owner_repo, "README.md", &b64, "bootstrap root readme").await?;
        }
        created.push("README.md".to_string());
    }

    Ok(created)
}

fn count_existing_episodes(existing: &[String], max: u32) -> u32 {
    existing.iter()
        .filter_map(|n| n.trim_start_matches('0').parse::<u32>().ok().filter(|&v| v >= 1))
        .filter(|&n| n <= max)
        .count() as u32
}

fn substitute_base_md(
    template: &str,
    meta: &AnimeMeta,
    repo_url: &str,
    episode_count_at_git: u32,
    season: u16,
    tl: &str,
    tlc: &str,
    ts: &str,
    qc: &str,
) -> String {
    let mut out = template.to_string();
    let pairs: Vec<(&str, String)> = vec![
        ("name", meta.name.clone()),
        ("slug", meta.slug.clone()),
        ("kind", kind_label(&meta.kind).to_string()),
        ("mal_id", meta.mal_id.to_string()),
        ("episode_count", meta.episode_count.to_string()),
        ("year", meta.year.map(|y| y.to_string()).unwrap_or_default()),
        ("repo_url", repo_url.to_string()),
        ("episode_count_at_git", episode_count_at_git.to_string()),
        ("season", season.to_string()),
        ("tl", tl.to_string()),
        ("tlc", tlc.to_string()),
        ("ts", ts.to_string()),
        ("qc", qc.to_string()),
    ];
    for (key, val) in &pairs {
        out = out.replace(&format!("%{}%", key), val);
    }
    out
}

async fn read_server_meta(server_id: u64) -> Result<(String, String, String), String> {
    let path = format!("DB/config/{}/meta.pandora", server_id);
    let s = tokio::fs::read_to_string(&path).await.map_err(|e| e.to_string())?;
    let mut lines = s.lines();
    let lang = lines.next().unwrap_or("tr").to_string();
    let forgejo = lines.next().unwrap_or("").to_string();
    let _channel_id = lines.next().unwrap_or("").to_string();
    let api_key = lines.next().unwrap_or("").to_string();
    Ok((lang, forgejo, api_key))
}

fn kind_label(k: &AnimeKind) -> &'static str {
    match k {
        AnimeKind::Movie => "Movie",
        AnimeKind::MultiEpisode => "MultiEpisode",
    }
}

async fn try_rename_channel_to_anime(ctx: &Context, channel_id: serenity::all::ChannelId, name: &str) -> Option<String> {
    let ch = channel_id.to_channel(&ctx.http).await.ok()?;
    let kind = match &ch {
        serenity::all::Channel::Guild(g) => g.kind,
        _ => return None,
    };
    let renamable = matches!(kind,
        ChannelType::Text
        | ChannelType::News
        | ChannelType::NewsThread
        | ChannelType::PublicThread
        | ChannelType::PrivateThread
        | ChannelType::Forum
    );
    if !renamable {
        return None;
    }
    let new_name: String = if name.chars().count() > 100 {
        name.chars().take(100).collect()
    } else {
        name.to_string()
    };
    channel_id.edit(&ctx.http, EditChannel::new().name(&new_name)).await.ok()?;
    Some(new_name)
}

async fn run_attach_or_init(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    is_init: bool,
) {
    let label = if is_init { "/init" } else { "/attach" };

    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: {} can only be used in a server", label))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let channel_id = command.channel_id.get();

    let mal_url = match command.data.options.iter()
        .find(|opt| opt.name == "mal")
        .and_then(|opt| opt.value.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(u) => u.to_string(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: `mal` is required for {}", label))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let repo_arg = command.data.options.iter()
        .find(|opt| opt.name == "repo")
        .and_then(|opt| opt.value.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    if !is_init && repo_arg.is_none() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: `repo` is required for /attach")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let season_input = command.data.options.iter()
        .find(|opt| opt.name == "season")
        .and_then(|opt| opt.value.as_i64())
        .unwrap_or(1);
    if season_input < 1 || season_input > u16::MAX as i64 {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: `season` must be between 1 and 65535.")
                .ephemeral(true)
        )).await.ok();
        return;
    }
    let season = season_input as u16;

    let tl = read_credit_option(command, "tl");
    let tlc = read_credit_option(command, "tlc");
    let ts = read_credit_option(command, "ts");
    let qc = read_credit_option(command, "qc");

    let existing = read_channel_meta(server_id, channel_id);

    let (_lang, forgejo_base, api_key) = match read_server_meta(server_id).await {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to read server meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    if forgejo_base.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: server has no forgejo org configured. Run `/configure` first.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Working...")
    )).await.ok();
    let mut response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return,
    };

    let meta = match fetch_anime(&mal_url).await {
        Ok(m) => m,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("MAL fetch failed: {}", e))).await;
            return;
        }
    };

    if let Some(eid) = existing.mal_id {
        if eid != meta.mal_id {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!(
                "Channel is already attached to `{}`. Use a different channel to attach a new anime.",
                existing.name.unwrap_or_default()
            ))).await;
            return;
        }
    }

    let fg = match Forgejo::new(forgejo_base.clone(), api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    let (owner_repo, repo_url) = if is_init {
        let repo_slug = meta.slug.replace('-', "_");
        let or = format!("{}/{}", fg.org, repo_slug);
        let url = match fg.create_repo(&repo_slug).await {
            Ok(u) => u,
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new().content(format!("create_repo failed: {}", e))).await;
                return;
            }
        };
        (or, url)
    } else {
        let repo_url = repo_arg.unwrap();
        let (owner, repo) = match parse_repo_url(&repo_url) {
            Ok(t) => t,
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Bad repo URL: {}", e))).await;
                return;
            }
        };
        (format!("{}/{}", owner, repo), repo_url)
    };

    let existing_root = match fg.list_contents(&owner_repo, "").await {
        Ok(v) => v,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("list_contents failed: {}", e))).await;
            return;
        }
    };

    let episode_count_at_git = count_existing_episodes(&existing_root, meta.episode_count);

    let base_md = tokio::fs::read_to_string(format!("DB/config/{}/base.md", server_id))
        .await
        .ok()
        .map(|t| substitute_base_md(&t, &meta, &repo_url, episode_count_at_git, season, &tl, &tlc, &ts, &qc));

    let created = match bootstrap_repo(&fg, &owner_repo, &meta, base_md, existing_root).await {
        Ok(v) => v,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Bootstrap failed: {}", e))).await;
            return;
        }
    };

    let mut renamed_files: Vec<String> = Vec::new();
    if !is_init {
        let safe_name = meta.name.replace('/', "-");
        for n in 1..=meta.episode_count {
            let folder = pad2(n);
            let entries = match fg.list_contents(&owner_repo, &folder).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ass_files: Vec<String> = entries.into_iter()
                .filter(|name| name.to_lowercase().ends_with(".ass"))
                .collect();
            if ass_files.len() != 1 {
                continue;
            }
            let old_name = ass_files.into_iter().next().unwrap();
            let old_path = format!("{}/{}", folder, old_name);
            let new_name = format!("TL - {} - E{:02}.ass", safe_name, n);
            let new_path = format!("{}/{}", folder, new_name);
            if old_path == new_path {
                continue;
            }
            if let Err(e) = fg.move_file(&owner_repo, &old_path, &new_path, "attach: rename to standard filename").await {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("move_file failed ({}): {}", old_path, e))).await;
                return;
            }
            renamed_files.push(format!("`{}` -> `{}`", folder, new_name));
        }
    }

    let new_meta = ChannelMeta {
        mal_id: Some(meta.mal_id),
        kind: Some(kind_label(&meta.kind).to_string()),
        name: Some(meta.name.clone()),
        slug: Some(meta.slug.clone()),
        episode_count: Some(meta.episode_count),
        repo_url: Some(repo_url.clone()),
        episode_count_at_git: Some(episode_count_at_git),
        year: meta.year,
        season: season,
        tl: tl.clone(),
        tlc: tlc.clone(),
        ts: ts.clone(),
        qc: qc.clone(),
    };
    if let Err(e) = write_channel_meta(server_id, channel_id, &new_meta).await {
        let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Failed to save channel meta: {}", e))).await;
        return;
    }

    let created_list = if created.is_empty() {
        "_none — repo already had all folders and README_".to_string()
    } else {
        created.join(", ")
    };
    let renamed = try_rename_channel_to_anime(ctx, command.channel_id, &meta.name).await;
    let rename_line = match &renamed {
        Some(n) => format!("\nChannel renamed: `{}`", n),
        None => String::new(),
    };
    let body = format!(
        "**{}** — attached to this channel.\nName: `{}`\nSlug: `{}`\nKind: `{}`\nEpisodes: `{}`\nRepo: <{}>\nCreated/updated: {}{}",
        label, meta.name, meta.slug, kind_label(&meta.kind), meta.episode_count, repo_url, created_list, rename_line,
    );
    let _ = response_msg.edit(ctx, EditMessage::new().content(body)).await;
}

pub async fn handle_init(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, true).await;
}

pub async fn handle_attach(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, false).await;
}

pub async fn handle_destruct(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: /destruct can only be used in a server")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let channel_id = command.channel_id.get();

    let meta = read_channel_meta(server_id, channel_id);
    let repo_url = match meta.repo_url.clone().filter(|s| !s.is_empty()) {
        Some(u) => u,
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: this channel is not attached to an anime. Run `/init` or `/attach` first.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let (owner, repo) = match parse_repo_url(&repo_url) {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: bad repo URL in meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let owner_repo = format!("{}/{}", owner, repo);
    let anime_name = meta.name.clone().unwrap_or_default();

    let (_lang, forgejo_base, api_key) = match read_server_meta(server_id).await {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: failed to read server meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    if forgejo_base.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: server has no forgejo org configured. Run `/configure` first.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Working…")
    )).await.ok();
    let mut response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return,
    };

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    match fg.delete_repo(&owner_repo).await {
        Ok(()) => {
            let _ = tokio::fs::remove_file(meta_path(server_id, channel_id)).await;
            let name_line = if anime_name.is_empty() {
                String::new()
            } else {
                format!(" (`{}`)", anime_name)
            };
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Deleted repo `{}`{}.\nChannel detached from this anime.", owner_repo, name_line))).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("delete_repo failed: {}", e))).await;
        }
    }
}

pub async fn handle_detach(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: /detach can only be used in a server")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let channel_id = command.channel_id.get();

    let meta = read_channel_meta(server_id, channel_id);
    if meta.repo_url.as_deref().map_or(true, str::is_empty) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: this channel is not attached to an anime.")
                .ephemeral(true)
        )).await.ok();
        return;
    }
    let anime_name = meta.name.clone().unwrap_or_default();
    let repo_url = meta.repo_url.clone().unwrap_or_default();

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Working…")
    )).await.ok();
    let mut response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return,
    };

    let _ = tokio::fs::remove_file(meta_path(server_id, channel_id)).await;

    let name_line = if anime_name.is_empty() {
        String::new()
    } else {
        format!(" (`{}`)", anime_name)
    };
    let _ = response_msg.edit(ctx, EditMessage::new()
        .content(format!("Detached channel from{}.\nRepo `{}` is left untouched.", name_line, repo_url))).await;
}

enum JobKind { TL, TLC, TS }

fn parse_job_kind(s: &str) -> Option<JobKind> {
    match s {
        "TL" => Some(JobKind::TL),
        "TLC" => Some(JobKind::TLC),
        "TS" => Some(JobKind::TS),
        _ => None,
    }
}

async fn extract_zip_root_ass(bytes: &[u8], dest: &Path) -> Result<Option<PathBuf>, String> {
    use async_zip::base::read::stream::ZipFileReader;
    use futures_lite::io::AsyncReadExt;
    use tokio::io::{AsyncWriteExt, BufReader};

    let tmp = std::env::temp_dir().join(format!("pandora_job_zip_{}.zip",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()));

    let result = async {
        {
            let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
            f.write_all(bytes).await.map_err(|e| e.to_string())?;
            f.sync_all().await.map_err(|e| e.to_string())?;
        }
        let f = tokio::fs::File::open(&tmp).await.map_err(|e| e.to_string())?;
        let mut zip = ZipFileReader::with_tokio(BufReader::new(f));

        let mut found: Option<PathBuf> = None;
        let mut count: usize = 0;

        loop {
            let mut entry = match zip.next_with_entry().await.map_err(|e| format!("zip: {}", e))? {
                Some(e) => e,
                None => break,
            };
            let filename = entry.reader().entry().filename().as_str()
                .map_err(|e| format!("zip filename: {}", e))?
                .to_string();
            let is_root = !filename.contains('/');
            let is_ass = filename.to_lowercase().ends_with(".ass");

            if is_root && is_ass {
                count += 1;
                if count > 1 {
                    return Ok(None);
                }
                let mut data = Vec::new();
                entry.reader_mut().read_to_end(&mut data).await
                    .map_err(|e| format!("zip read: {}", e))?;
                let out_path = dest.join(&filename);
                tokio::fs::write(&out_path, &data).await.map_err(|e| e.to_string())?;
                found = Some(out_path);
            }

            zip = entry.skip().await.map_err(|e| format!("zip skip: {}", e))?;
        }

        Ok(found)
    }.await;

    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

fn base64_decode_bytes(input: &str) -> Result<Vec<u8>, String> {
    const ALPH: [u8; 128] = {
        let mut a = [255u8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < chars.len() {
            a[chars[i] as usize] = i as u8;
            i += 1;
        }
        a
    };
    let cleaned: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if cleaned.len() % 4 != 0 {
        return Err(format!("base64: invalid length {}", cleaned.len()));
    }
    let mut out: Vec<u8> = Vec::with_capacity(cleaned.len() / 4 * 3);
    let mut i = 0;
    while i < cleaned.len() {
        let c0 = cleaned[i];
        let c1 = cleaned[i + 1];
        let c2 = cleaned[i + 2];
        let c3 = cleaned[i + 3];
        let pad2 = c2 == b'=';
        let pad3 = c3 == b'=';
        let v0 = ALPH[c0 as usize];
        let v1 = ALPH[c1 as usize];
        if v0 == 255 || v1 == 255 {
            return Err(format!("base64: invalid char at {}", i));
        }
        if !pad2 {
            let v2 = ALPH[c2 as usize];
            if v2 == 255 {
                return Err(format!("base64: invalid char at {}", i + 2));
            }
            out.push((v0 << 2) | (v1 >> 4));
            if !pad3 {
                let v3 = ALPH[c3 as usize];
                if v3 == 255 {
                    return Err(format!("base64: invalid char at {}", i + 3));
                }
                out.push((v1 << 4) | (v2 >> 2));
                out.push((v2 << 6) | v3);
            } else {
                out.push((v1 << 4) | (v2 >> 2));
            }
        } else {
            out.push((v0 << 2) | (v1 >> 4));
        }
        i += 4;
    }
    Ok(out)
}

pub async fn handle_job(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let job_kind = match command.data.options.iter()
        .find(|opt| opt.name == "type")
        .and_then(|opt| opt.value.as_str())
        .and_then(parse_job_kind)
    {
        Some(k) => k,
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `type` must be TL, TLC, or TS.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let episode = match command.data.options.iter()
        .find(|opt| opt.name == "episode")
        .and_then(|opt| opt.value.as_i64())
    {
        Some(n) if n >= 1 && n <= u32::MAX as i64 => n as u32,
        _ => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `episode` must be a positive integer.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let attachment_id = command.data.options.iter()
        .find(|opt| opt.name == "subtitle")
        .and_then(|opt| opt.value.as_attachment_id());
    let attachment = match attachment_id.and_then(|id| command.data.resolved.attachments.get(&id)) {
        Some(a) => a,
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `subtitle` attachment is required.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let custom_commit = command.data.options.iter()
        .find(|opt| opt.name == "commit")
        .and_then(|opt| opt.value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();

    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: /job can only be used in a server")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let channel_id = command.channel_id.get();

    let meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: this channel is not attached to an anime. Run `/init` or `/attach` first.")
                .ephemeral(true)
        )).await.ok();
        return;
    }
    let name = meta.name.clone().unwrap_or_default();
    let max_ep = meta.episode_count.unwrap_or(0);
    if episode < 1 || episode > max_ep {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: `episode` must be between 1 and {}.", max_ep))
                .ephemeral(true)
        )).await.ok();
        return;
    }
    let repo_url = meta.repo_url.clone().unwrap_or_default();
    let (owner, repo) = match parse_repo_url(&repo_url) {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: bad repo URL in meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let owner_repo = format!("{}/{}", owner, repo);

    let (_lang, forgejo_base, api_key) = match read_server_meta(server_id).await {
        Ok(t) => t,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: failed to read server meta: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    if forgejo_base.is_empty() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: server has no forgejo org configured. Run `/configure` first.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("Working…")
    )).await.ok();
    let mut response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return,
    };

    let attachment_bytes = match attachment.download().await {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to download attachment: {}", e))).await;
            return;
        }
    };

    let job_id = response_msg.id.get();
    let job_dir = format!("DB/saved_data/{}", job_id);
    if let Err(e) = tokio::fs::create_dir_all(&job_dir).await {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to create job dir: {}", e))).await;
        return;
    }
    let input_path = format!("{}/input.ass", job_dir);
    let output_path = format!("{}/output.ass", job_dir);

    let attachment_name = attachment.filename.to_lowercase();
    if attachment_name.ends_with(".ass") {
        if let Err(e) = tokio::fs::write(&input_path, &attachment_bytes).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to write input: {}", e))).await;
            return;
        }
    } else if attachment_name.ends_with(".zip") {
        let extract_dir = format!("{}/extract", job_dir);
        if let Err(e) = tokio::fs::create_dir_all(&extract_dir).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to create extract dir: {}", e))).await;
            return;
        }
        match extract_zip_root_ass(&attachment_bytes, &PathBuf::from(&extract_dir)).await {
            Ok(Some(src)) => {
                if let Err(e) = tokio::fs::copy(&src, &input_path).await {
                    let _ = response_msg.edit(ctx, EditMessage::new()
                        .content(format!("Failed to copy extracted .ass: {}", e))).await;
                    return;
                }
            }
            Ok(None) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content("Error: zip must contain exactly one .ass file at the root.")).await;
                return;
            }
            Err(e) => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content(format!("Zip extraction failed: {}", e))).await;
                return;
            }
        }
    } else {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content("Error: unsupported subtitle file type. Use .ass or .zip.")).await;
        return;
    }

    let mut warnings: Vec<String> = Vec::new();
    let needs_pnass = matches!(job_kind, JobKind::TL | JobKind::TLC);
    if needs_pnass {
        let pnass_path = match get_pandora_env().get(PNASS) {
            Some(p) if !p.is_empty() => p.clone(),
            _ => {
                let _ = response_msg.edit(ctx, EditMessage::new()
                    .content("Error: PNASS binary path is not set in DB/config/global/environment/env.pandora.")).await;
                return;
            }
        };
        let mut proto = Protocol::new(vec![1]);
        let result = run_tool(
            &pnass_path,
            PNASS_LAYER,
            &HashMap::from([
                ("INPUT", PathValue::from(input_path.clone())),
                ("OUTPUT", PathValue::from(output_path.clone())),
            ]),
            job_id,
            &mut proto,
            |data| {
                match data.get(0).and_then(|v| v.as_str()) {
                    Some("4") => {
                        if let Some(line) = data.get(1).and_then(|v| v.as_str()) {
                            warnings.push(line.to_string());
                        }
                    }
                    _ => {}
                }
                None
            },
        ).await;
        if !matches!(result, ToolResult::Success) {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("ASS normalisation failed (warnings so far: {}).", warnings.len()))).await;
            return;
        }
    } else {
        if let Err(e) = tokio::fs::copy(&input_path, &output_path).await {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to copy input to output: {}", e))).await;
            return;
        }
    }

    let title = if name.is_empty() { owner.clone() } else { format!("{} - {}", owner, name) };
    let mut sub = SubstationAlpha::load(PathBuf::from(&output_path), false).await;
    sub.script_info.title = title;
    if sub.dump_to_file(PathBuf::from(&output_path)).await.is_err() {
        let _ = response_msg.edit(ctx, EditMessage::new()
            .content(format!("Failed to rewrite ASS title."))).await;
        return;
    }

    let output_bytes = match tokio::fs::read(&output_path).await {
        Ok(b) => b,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Failed to read output: {}", e))).await;
            return;
        }
    };
    let b64 = base64_encode_bytes(&output_bytes);

    let (file_type_label, prefix, default_msg) = match job_kind {
        JobKind::TL  => ("TL",  "TL",  "Translation"),
        JobKind::TLC => ("TL",  "TLC", "Edit"),
        JobKind::TS  => ("TS",  "TS",  "Typeset"),
    };
    let commit_msg = if custom_commit.is_empty() {
        default_msg.to_string()
    } else {
        format!("[{}] {}", prefix, custom_commit)
    };
    let safe_name = name.replace('/', "-");
    let file_name = format!("{} - {} - E{:02}.ass",
        file_type_label, safe_name, episode);
    let repo_path = format!("{}/{}", pad2(episode), file_name);

    let fg = match Forgejo::new(forgejo_base, api_key) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };
    match fg.upsert_file(&owner_repo, &repo_path, &b64, &commit_msg).await {
        Ok(()) => {
            let embed = CreateEmbed::new()
                .title("Job complete")
                .field("Repo", format!("`{}`", owner_repo), true)
                .field("File", format!("`{}`", repo_path), true)
                .field("Job", format!("`{}`", job_id), true)
                .field("Commit Message", format!("`{}`", commit_msg), false)
                .field("Warnings", format_warnings_field(&warnings), false);
            let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
        }
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new()
                .content(format!("Upload failed: {}", e))).await;
        }
    }
}

fn format_warnings_field(warnings: &[String]) -> String {
    if warnings.is_empty() {
        return "None".to_string();
    }
    const LIMIT: usize = 1000;
    let mut out = String::new();
    let mut count = 0usize;
    for w in warnings {
        let piece = format!("- {}\n", w);
        if out.len() + piece.len() > LIMIT {
            out.push_str(&format!("…and {} more", warnings.len() - count));
            return out;
        }
        out.push_str(&piece);
        count += 1;
    }
    if out.len() > 1024 {
        out.truncate(1021);
        out.push_str("…");
    }
    out
}

pub async fn handle_gitcode(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    let subtitle_url = match command.data.options.iter()
        .find(|opt| opt.name == "subtitle_url")
        .and_then(|opt| opt.value.as_str())
    {
        Some(url) if !url.is_empty() => url.to_string(),
        _ => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: subtitle_url is required")
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    let normalized = github_blob_to_raw(&subtitle_url);
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to build HTTP client: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    let attachment_bytes = match client.get(&normalized).send().await {
        Ok(resp) => match resp.bytes().await {
            Ok(b) => b.to_vec(),
            Err(e) => {
                command.create_response(ctx, CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("Failed to fetch subtitle: {}", e))
                        .ephemeral(true)
                )).await.ok();
                return None;
            }
        },
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to fetch subtitle: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    response_msg.react(ctx, '❌').await.ok();

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Encode,
        response_msg.id.get(),
        preset,
        nyaaise(&torrent_url),
        attachment_bytes,
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
    ))
}

fn github_blob_to_raw(url: &str) -> String {
    let re = Regex::new(r"^https?://github\.com/([^/]+)/([^/]+)/blob/([^/]+)/(.+)$").unwrap();
    if let Some(caps) = re.captures(url) {
        format!("https://raw.githubusercontent.com/{}/{}/{}/{}",
            caps.get(1).unwrap().as_str(),
            caps.get(2).unwrap().as_str(),
            caps.get(3).unwrap().as_str(),
            caps.get(4).unwrap().as_str())
    } else {
        url.to_string()
    }
}

pub async fn handle_configure(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: /configure can only be used in a server")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let language = match command.data.options.iter()
        .find(|opt| opt.name == "language")
        .and_then(|opt| opt.value.as_str())
    {
        Some(l) if matches!(l, "EN" | "TR" | "JP") => l.to_string(),
        Some(other) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: language `{}` is not one of EN/TR/JP", other))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: language is required")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let forgejo = match command.data.options.iter()
        .find(|opt| opt.name == "forgejo")
        .and_then(|opt| opt.value.as_str())
    {
        Some(u) if u.is_empty() => String::new(),
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.trim_end_matches('/').to_string(),
        Some(other) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: forgejo `{}` must be an http(s) URL", other))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
        None => String::new(),
    };

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to create config dir: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let existing_api_key = std::fs::read_to_string(dir.join("meta.pandora"))
        .ok()
        .and_then(|s| s.lines().nth(3).map(str::to_string))
        .unwrap_or_default();

    let new_api_key = command.data.options.iter()
        .find(|opt| opt.name == "api_key")
        .and_then(|opt| opt.value.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing_api_key)
        .to_string();

    let body = format!("{}\n{}\n{}\n{}\n", language, forgejo, command.channel_id.get(), new_api_key);
    let path = dir.join("meta.pandora");
    if let Err(e) = tokio::fs::write(&path, body).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write meta.pandora: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let forgejo_display = if forgejo.is_empty() { "(unset)".to_string() } else { format!("`{}`", forgejo) };
    let api_key_display = if new_api_key.is_empty() { "(unset)".to_string() } else { "(set)".to_string() };
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Configured server `{}` — language: {}, forgejo: {}, forgejo api_key: {}, announcement channel: <#{}>",
                server_id, language, forgejo_display, api_key_display, command.channel_id.get()))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_readmebase(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command.guild_id {
        Some(g) => g.get(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: /readmebase can only be used in a server")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let attachment_id = command.data.options.iter()
        .find(|opt| opt.name == "file")
        .and_then(|opt| opt.value.as_attachment_id());
    let attachment = match attachment_id.and_then(|id| command.data.resolved.attachments.get(&id)) {
        Some(a) => a,
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `file` attachment is required.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let attachment_bytes = match attachment.download().await {
        Ok(b) => b,
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to download attachment: {}", e))
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    let dir = std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string());
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to create config dir: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let path = dir.join("base.md");
    if let Err(e) = tokio::fs::write(&path, &attachment_bytes).await {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to write base.md: {}", e))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Set `base.md` for server `{}` ({} bytes, from `{}`).",
                server_id, attachment_bytes.len(), attachment.filename))
            .ephemeral(true)
    )).await.ok();
}

pub async fn handle_auth(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let user_id = match command.data.options.iter()
        .find(|opt| opt.name == "user_id")
        .and_then(|opt| opt.value.as_str())
    {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `user_id` is required.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let level = command.data.options.iter()
        .find(|opt| opt.name == "level")
        .and_then(|opt| opt.value.as_str())
        .unwrap_or("authorize.pandora")
        .to_string();

    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: your level does not outrank `{}`.", level))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let mut to_add = user_id.clone();
    if add_env(&perm_path(&level), &mut to_add) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Authorized <@{}> at `{}`.", user_id, level))
                .ephemeral(true)
        )).await.ok();
    } else {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Failed to authorize: could not open `{}` for writing.", level))
                .ephemeral(true)
        )).await.ok();
    }
}

pub async fn handle_remove(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let user_id = match command.data.options.iter()
        .find(|opt| opt.name == "user_id")
        .and_then(|opt| opt.value.as_str())
    {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `user_id` is required.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };
    let level = match command.data.options.iter()
        .find(|opt| opt.name == "level")
        .and_then(|opt| opt.value.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(s) => s.to_string(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: `level` is required.")
                    .ephemeral(true)
            )).await.ok();
            return;
        }
    };

    if !has_level_at_least(command.user.id.get(), level_rank(&level)) {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content(format!("Error: your level does not outrank `{}`.", level))
                .ephemeral(true)
        )).await.ok();
        return;
    }

    match remove_env(&perm_path(&level), &user_id) {
        Ok(true) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Removed <@{}> from `{}`.", user_id, level))
                    .ephemeral(true)
            )).await.ok();
        }
        Ok(false) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("<@{}> was not in `{}`.", user_id, level))
                    .ephemeral(true)
            )).await.ok();
        }
        Err(e) => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Failed to remove: {}", e))
                    .ephemeral(true)
            )).await.ok();
        }
    }
}

pub async fn handle_interaction(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    let attachment_id = command.data.options.iter()
        .find(|opt| opt.name == "subtitle")
        .and_then(|opt| opt.value.as_attachment_id());

    let attachment_bytes = match attachment_id.and_then(|id| command.data.resolved.attachments.get(&id)) {
        Some(att) => match att.download().await {
            Ok(b) => b,
            Err(e) => {
                command.create_response(ctx, CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("Failed to download attachment: {}", e))
                        .ephemeral(true)
                )).await.ok();
                return None;
            }
        },
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: Subtitle file is required")
                    .ephemeral(true)
            )).await.ok();
            return None;
        }
    };

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new().content("...")
    )).await.ok();

    let response_msg = match command.get_response(&ctx.http).await {
        Ok(m) => m,
        Err(_) => return None,
    };

    response_msg.react(ctx, '❌').await.ok();

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Encode,
        response_msg.id.get(),
        preset,
        nyaaise(&torrent_url),
        attachment_bytes,
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
    ))
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, context: Context, msg: Message) {
        let parts: Vec<&str> = msg.content.split_whitespace().collect();
        if parts.is_empty() { return; }
        if !is_authorized(parts[0], msg.author.id.get()) { return; }

        match parts[0] {
            "!enc" => {
                msg.reply(context, "Lütfen yeni /encode komutunu kullanın.").await.unwrap();
            }
            _ => {}
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            if !is_authorized(command.data.name.as_str(), command.user.id.get()) {
                println!("[gate] BLOCKED user={} cmd={}", command.user.id.get(), command.data.name.as_str());
                command.create_response(&ctx, CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("Yetkisiz işlem.\nGeliştiricimden izin isteyin.")
                        .ephemeral(true)
                )).await.ok();
                return;
            }
            println!("[gate] ALLOWED user={} cmd={}", command.user.id.get(), command.data.name.as_str());
            match command.data.name.as_str() {
                "encode" => {
                    let torrent_url = match command.data.options.iter()
                        .find(|opt| opt.name == "torrent")
                        .and_then(|opt| opt.value.as_str())
                    {
                        Some(url) if !url.is_empty() => url.to_string(),
                        _ => {
                            command.create_response(&ctx.http, CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Error: Torrent URL is required")
                                    .ephemeral(true)
                            )).await.ok();
                            return;
                        }
                    };

                    let candidates = command.data.options.iter()
                        .find(|opt| opt.name == "concat")
                        .and_then(|opt| opt.value.as_str())
                        .and_then(|group| self.intros.resolve(group));

                    let preset = match command.data.options.iter()
                        .find(|opt| opt.name == "preset")
                        .and_then(|opt| opt.value.as_str())
                        .unwrap_or("standard")
                    {
                        "gpu"           => Preset::Standard(candidates),
                        "standard"      => Preset::Standard(candidates),
                        "dummy"         => Preset::Dummy(candidates),
                        _               => Preset::PseudoLossless(candidates),
                    };

                    if let Some(job) = handle_interaction(&ctx, &command, torrent_url, preset).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "probe" => {
                    let torrent_url = match command.data.options.iter()
                        .find(|opt| opt.name == "torrent")
                        .and_then(|opt| opt.value.as_str())
                    {
                        Some(url) if !url.is_empty() => url.to_string(),
                        _ => {
                            command.create_response(&ctx, CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Error: Torrent URL is required")
                                    .ephemeral(true)
                            )).await.ok();
                            return;
                        }
                    };

                    if let Some(job) = handle_probe(&ctx, &command, torrent_url).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "pancode" => {
                    let probe_job_id = match command.data.options.iter()
                        .find(|opt| opt.name == "job_id")
                        .and_then(|opt| opt.value.as_str())
                    {
                        Some(id) => match id.parse::<u64>() {
                            Ok(x) => x,
                            Err(_) => {return;}
                        },
                        None => {
                            command.create_response(&ctx, CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Error: job_id is required")
                                    .ephemeral(true)
                            )).await.ok();
                            return;
                        }
                    };

                    let file_index = match command.data.options.iter()
                        .find(|opt| opt.name == "index")
                        .and_then(|opt| opt.value.as_i64())
                    {
                        Some(i) => i as u64,
                        None => {
                            command.create_response(&ctx, CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Error: file index is required")
                                    .ephemeral(true)
                            )).await.ok();
                            return;
                        }
                    };

                    let candidates = command.data.options.iter()
                        .find(|opt| opt.name == "concat")
                        .and_then(|opt| opt.value.as_str())
                        .and_then(|group| self.intros.resolve(group));

                    let preset = match command.data.options.iter()
                        .find(|opt| opt.name == "preset")
                        .and_then(|opt| opt.value.as_str())
                        .unwrap_or("standard")
                    {
                        "gpu"      => Preset::Standard(candidates),
                        "standard" => Preset::Standard(candidates),
                        "dummy"    => Preset::Dummy(candidates),
                        _          => Preset::PseudoLossless(candidates),
                    };

                    if let Some(mut job) = handle_interaction(&ctx, &command, String::new(), preset).await {
                        // Override job type and carry the probe linkage via job_id
                        job.job_type = JobType::Pancode;
                        job.probe_job_id = Some(probe_job_id);
                        job.probe_file_index = Some(file_index);
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "backup" => {
                    let torrent_url = match command.data.options.iter()
                        .find(|opt| opt.name == "torrent")
                        .and_then(|opt| opt.value.as_str())
                    {
                        Some(url) if !url.is_empty() => url.to_string(),
                        _ => {
                            command.create_response(&ctx, CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Error: Torrent URL is required")
                                    .ephemeral(true)
                            )).await.ok();
                            return;
                        }
                    };

                    if let Some(job) = handle_backup(&ctx, &command, torrent_url).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "configure" => {
                    handle_configure(&ctx, &command).await;
                }
                "readmebase" => {
                    handle_readmebase(&ctx, &command).await;
                }
                "auth" => {
                    handle_auth(&ctx, &command).await;
                }
                "remove" => {
                    handle_remove(&ctx, &command).await;
                }
                "hearts" => {
                    command.create_response(&ctx, CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new().content("...")
                    )).await.ok();
                    let response_msg = match command.get_response(&ctx.http).await {
                        Ok(m) => m,
                        Err(_) => return,
                    };
                    self.tx.send(JobClass::HalfJob(HalfJob::new_hearts(
                        command.user.id.get(),
                        command.channel_id.get(),
                        response_msg.id.get(),
                        ctx.clone(),
                        response_msg,
                    ))).await.ok();
                }
                "attach" => {
                    handle_attach(&ctx, &command).await;
                }
                "init" => {
                    handle_init(&ctx, &command).await;
                }
                "destruct" => {
                    handle_destruct(&ctx, &command).await;
                }
                "detach" => {
                    handle_detach(&ctx, &command).await;
                }
                "smartcode" => {
                    if let Some(job) = handle_smartcode(&ctx, &command, &self.intros).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "source" => {
                    handle_source(&ctx, &command).await;
                }
                "job" => {
                    handle_job(&ctx, &command).await;
                }
                "gitcode" => {
                    let torrent_url = match command.data.options.iter()
                        .find(|opt| opt.name == "torrent")
                        .and_then(|opt| opt.value.as_str())
                    {
                        Some(url) if !url.is_empty() => url.to_string(),
                        _ => {
                            command.create_response(&ctx.http, CreateInteractionResponse::Message(
                                CreateInteractionResponseMessage::new()
                                    .content("Error: Torrent URL is required")
                                    .ephemeral(true)
                            )).await.ok();
                            return;
                        }
                    };

                    let candidates = command.data.options.iter()
                        .find(|opt| opt.name == "concat")
                        .and_then(|opt| opt.value.as_str())
                        .and_then(|group| self.intros.resolve(group));

                    let preset = match command.data.options.iter()
                        .find(|opt| opt.name == "preset")
                        .and_then(|opt| opt.value.as_str())
                        .unwrap_or("standard")
                    {
                        "gpu"           => Preset::Standard(candidates),
                        "standard"      => Preset::Standard(candidates),
                        "dummy"         => Preset::Dummy(candidates),
                        _               => Preset::PseudoLossless(candidates),
                    };

                    if let Some(job) = handle_gitcode(&ctx, &command, torrent_url, preset).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "gitsync" => {
                    command.create_response(&ctx, CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new().content("Tüm işlemler kapatılıyor.")
                    )).await.ok();
                    let response_msg = match command.get_response(&ctx.http).await {
                        Ok(m) => m,
                        Err(_) => return,
                    };
                    self.tx.send(JobClass::HalfJob(HalfJob::new_gitsync(
                        command.user.id.get(),
                        command.channel_id.get(),
                        response_msg.id.get(),
                        ctx.clone(),
                        response_msg,
                    ))).await.ok();
                }
                _ => {}
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        println!("Bot ID: {}", ready.user.id);
        println!("Serving {} guilds", ready.guilds.len());

        ctx.set_presence(Some(ActivityData::custom("Pandora is active.")), OnlineStatus::Online);

        let mut concat_option = CreateCommandOption::new(
            CommandOptionType::String,
            "concat",
            "Intro"
        ).required(false);

        for group_name in self.intros.groups.keys() {
            concat_option = concat_option.add_string_choice(group_name, group_name);
        }

        let commands = vec![
            CreateCommand::new("encode")
                .description("Encode a video with subtitle")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "torrent", "Torrent URL, magnet link, or Google Drive link")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Attachment, "subtitle", "ASS subtitle file")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "preset", "Encoding preset")
                        .required(false)
                        .add_string_choice("Pseudo Lossless", "pseudolossless")
                        .add_string_choice("Standard x264", "standard")
                        .add_string_choice("GPU", "gpu")
                        .add_string_choice("DEV", "dummy")
                )
                .add_option(concat_option.clone()),
            CreateCommand::new("hearts")
                .description("Check the health of all worker threads"),
            CreateCommand::new("probe")
                .description("Download and ffprobe a torrent. Then can be used to encode with its own subtitle.")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "torrent", "Torrent URL or magnet link")
                        .required(true)
                ),
            CreateCommand::new("pancode")
                .description("Encode using a previously probed torrent")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "job_id", "Job ID from /probe result")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "index", "File index from probe results")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Attachment, "subtitle", "ASS subtitle file")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "preset", "Encoding preset")
                        .required(false)
                        .add_string_choice("Pseudo Lossless", "pseudolossless")
                        .add_string_choice("Standard x264", "standard")
                        .add_string_choice("GPU", "gpu")
                        .add_string_choice("DEV", "dummy")
                )
                .add_option(concat_option.clone()),
            CreateCommand::new("gitsync")
                .description("Sync with the git repo"),
            CreateCommand::new("backup")
                .description("Download torrent and upload MKV to GDrive without release")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "torrent", "Torrent URL or magnet link")
                        .required(true)
                ),
            CreateCommand::new("attach")
                .description("Attach a MyAnimeList anime to this channel and bootstrap an existing Forgejo repo")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "mal", "MyAnimeList link (e.g. https://myanimelist.net/anime/52991)")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "repo", "Forgejo repo link (e.g. https://git.einzu.fun/owner/repo)")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "season", "Season number (1 for the first season, 2 for a sequel, …). Defaults to 1.")
                        .required(false)
                        .min_int_value(1)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "tl", "Translator credit (defaults to `---`)")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "tlc", "Translation checker credit (defaults to `---`)")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "ts", "Typesetter credit (defaults to `---`)")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "qc", "Quality checker credit (defaults to `---`)")
                        .required(false)
                ),
            CreateCommand::new("init")
                .description("Attach a MyAnimeList anime to this channel and create a new Forgejo repo for it")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "mal", "MyAnimeList link (e.g. https://myanimelist.net/anime/52991)")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "season", "Season number (1 for the first season, 2 for a sequel, …). Defaults to 1.")
                        .required(false)
                        .min_int_value(1)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "tl", "Translator credit (defaults to `---`)")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "tlc", "Translation checker credit (defaults to `---`)")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "ts", "Typesetter credit (defaults to `---`)")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "qc", "Quality checker credit (defaults to `---`)")
                        .required(false)
                ),
            CreateCommand::new("destruct")
                .description("Delete the Forgejo repo of the attached anime and detach this channel"),
            CreateCommand::new("detach")
                .description("Detach this channel from its attached anime (the Forgejo repo is left untouched)"),
            CreateCommand::new("smartcode")
                .description("Merge the channel's attached TL and TS subtitles for an episode and encode a torrent")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "episode", "Episode number (1-based)")
                        .required(true)
                        .min_int_value(1)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "link", "Source link. Falls back to SOURCE.md if omitted.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "preset", "Encoding preset")
                        .required(false)
                        .add_string_choice("Pseudo Lossless", "pseudolossless")
                        .add_string_choice("Standard x264", "standard")
                        .add_string_choice("GPU", "gpu")
                        .add_string_choice("DEV", "dummy")
                )
                .add_option(concat_option.clone()),
            CreateCommand::new("source")
                .description("Write the SOURCE.md for an episode's folder in the attached repo")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "episode", "Episode number (1-based)")
                        .required(true)
                        .min_int_value(1)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "link", "Source link (torrent URL, magnet link, or Google Drive link)")
                        .required(true)
                ),
            CreateCommand::new("gitcode")
                .description("Encode with a subtitle fetched from a URL")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "torrent", "Torrent URL, magnet link, or Google Drive link")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "subtitle_url", "URL to an .ass subtitle file (raw or github blob)")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "preset", "Encoding preset")
                        .required(false)
                        .add_string_choice("Pseudo Lossless", "pseudolossless")
                        .add_string_choice("Standard x264", "standard")
                        .add_string_choice("GPU", "gpu")
                        .add_string_choice("DEV", "dummy")
                )
                .add_option(concat_option.clone()),
            CreateCommand::new("configure")
                .description("Configure this server (language, Forgejo base link, Forgejo API key)")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "language", "Bot language")
                        .required(true)
                        .add_string_choice("English", "EN")
                        .add_string_choice("Türkçe", "TR")
                        .add_string_choice("日本語", "JP")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "forgejo", "Forgejo base link (e.g. https://git.einzu.fun) — leave empty to unset")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "api_key", "Forgejo API token. Omit to keep the existing one.")
                        .required(false)
                ),
            CreateCommand::new("readmebase")
                .description("Set the base.md for this server (used as the README template when bootstrapping repos)")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Attachment, "file", "The base.md file")
                        .required(true)
                ),
            CreateCommand::new("auth")
                .description("Append a user id to an auth level file")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "user_id", "The Discord user id to authorize")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "level", "The auth level file. Defaults to `authorize.pandora`.")
                        .required(false)
                        .add_string_choice("Authorize", "authorize.pandora")
                        .add_string_choice("Upper", "upper.pandora")
                        .add_string_choice("Fansubber", "fansubber.pandora")
                        .add_string_choice("Admin", "admin.pandora")
                ),
            CreateCommand::new("remove")
                .description("Remove a user id from an auth level file")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "user_id", "The Discord user id to deauthorize")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "level", "The auth level file")
                        .required(true)
                        .add_string_choice("Authorize", "authorize.pandora")
                        .add_string_choice("Upper", "upper.pandora")
                        .add_string_choice("Fansubber", "fansubber.pandora")
                        .add_string_choice("Admin", "admin.pandora")
                ),
            CreateCommand::new("job")
                .description("Submit a single-episode job against the channel's attached anime")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "type", "Job type")
                        .required(true)
                        .add_string_choice("Translation", "TL")
                        .add_string_choice("Translation Check", "TLC")
                        .add_string_choice("Typeset", "TS")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "episode", "Episode number (1-based)")
                        .required(true)
                        .min_int_value(1)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Attachment, "subtitle", "The .ass or .zip file")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "commit", "Custom commit message (optional; will be prefixed with [TL]/[TLC]/[TS])")
                        .required(false)
                ),
        ];

        for guild in &ready.guilds {
            if let Err(why) = guild.id.set_commands(&ctx.http, commands.clone()).await {
                println!("Failed to register commands for guild {}: {}", guild.id, why);
            }
        }

        println!("Slash commands registered!");
    }

    async fn reaction_add(&self, ctx: Context, reaction: serenity::all::Reaction) {
        if let Some(user_id) = reaction.user_id {
            if user_id == ctx.cache.current_user().id { return; }
            if let serenity::all::ReactionType::Unicode(ref emoji) = reaction.emoji {
                if emoji == "❌" {
                    self.tx.send(JobClass::HalfJob(HalfJob::new_cancel(
                        user_id.get(),
                        reaction.channel_id.get(),
                        reaction.message_id.get()
                    ))).await.ok();
                }
            }
        }
    }
}

#[tokio::main]
async fn main() {
    migrate_pandora_files().await;
    let env = get_pandora_env();
    let (tx, rx): (Sender<JobClass>, Receiver<JobClass>) = channel(5);
    tokio::spawn(pn_worker(rx));
    let intros = IntrosConfig::load();
    println!("{:?}", intros);
    pandora_toolchain::pnworker::messages::init_language_files();
    let mut discord = Client::builder(env.get(TOKEN).cloned().unwrap_or_default(), GatewayIntents::all())
        .event_handler(Handler { tx, intros })
        .await
        .unwrap();

    if let Err(why) = discord.start().await {
        println!("{}", why);
    }
}
