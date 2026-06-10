use serenity::{
    Client,
    all::{ActivityData, ChannelType, CommandOptionType, Context, CreateEmbed, CreateMessage, EditInteractionResponse, EditMessage, GatewayIntents, Interaction, Message, OnlineStatus, Ready},
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
    core::{add_env, get_pandora_env, get_perm, remove_env, upsert_env},
    standard::{ENV_PATH, TOKEN},
};
use pandora_toolchain::libpnmal::{fetch_anime, AnimeMeta, AnimeKind};
use pandora_toolchain::libpnforgejo::{Forgejo, base64_encode, base64_encode_bytes};
use pandora_toolchain::pnworker::core::pn_worker;
use pandora_toolchain::libpnenv::standard::PNASS;
use pandora_toolchain::libkagami::core::{SubstationAlpha, find_fonts_with_roots};
use pandora_toolchain::libpnprotocol::core::Protocol;
use tokio::sync::mpsc::{channel, Sender, Receiver};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use regex::Regex;
use reqwest;

#[path = "../helpers/pndc.rs"]
mod pndc_helpers;
use pndc_helpers::*;
#[allow(dead_code)]
#[path = "../helpers/handlers.rs"]
mod handlers;
use handlers::*;

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
        "encode" | "pancode" | "probe" | "backup" | "backupall" | "scrape" | "gitcode" | "smartcode" | "merge" | "source" => 0,
        "!enc" | "!encode" => 0,
        "job" | "!ts" => 1,
        "auth" | "remove" | "gitsync" | "hearts" | "configure" | "readmebase" | "addapi" | "font" | "!ban" | "!some" => 2,
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
            "!ts" => {
                handle_ts_message(&context, &msg, &parts).await;
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
                    let torrent_url = match required_trimmed_option(&ctx, &command, "torrent", "Torrent URL").await {
                        Some(url) => url,
                        None => return,
                    };
                    let preset = resolve_preset(&command, &self.intros);

                    if let Some(job) = handle_interaction(&ctx, &command, torrent_url, preset).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "probe" => {
                    let torrent_url = match required_trimmed_option(&ctx, &command, "torrent", "Torrent URL").await {
                        Some(url) => url,
                        None => return,
                    };

                    if let Some(job) = handle_probe(&ctx, &command, torrent_url).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "pancode" => {
                    let probe_job_id = match option_str(&command, "job_id") {
                        Some(id) => match id.parse::<u64>() {
                            Ok(x) => x,
                            Err(_) => {return;}
                        },
                        None => {
                            command_error(&ctx, &command, "Error: job_id is required").await;
                            return;
                        }
                    };

                    let file_index = match option_i64(&command, "index") {
                        Some(i) => i as u64,
                        None => {
                            command_error(&ctx, &command, "Error: file index is required").await;
                            return;
                        }
                    };
                    let preset = resolve_preset(&command, &self.intros);

                    if let Some(mut job) = handle_interaction(&ctx, &command, String::new(), preset).await {
                        // Override job type and carry the probe linkage via job_id
                        job.job_type = JobType::Pancode;
                        job.probe_job_id = Some(probe_job_id);
                        job.probe_file_index = Some(file_index);
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "backup" => {
                    let probe_job_id = match option_str(&command, "job_id") {
                        Some(id) => match id.parse::<u64>() {
                            Ok(x) => Some(x),
                            Err(_) => {
                                command_error(&ctx, &command, "Error: job_id must be a number").await;
                                return;
                            }
                        },
                        None => None,
                    };

                    let file_index = match probe_job_id {
                        Some(_) => match option_i64(&command, "index") {
                            Some(i) if i >= 0 => Some(i as u64),
                            _ => {
                                command_error(&ctx, &command, "Error: index is required when job_id is provided").await;
                                return;
                            }
                        },
                        None => None,
                    };

                    let torrent_url = match probe_job_id {
                        Some(_) => String::new(),
                        None => match required_trimmed_option(&ctx, &command, "torrent", "Torrent URL").await {
                            Some(url) => url,
                            None => return,
                        },
                    };

                    if let Some(mut job) = handle_backup(&ctx, &command, torrent_url).await {
                        job.probe_job_id = probe_job_id;
                        job.probe_file_index = file_index;
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "backupall" => {
                    let torrent_url = match required_trimmed_option(&ctx, &command, "torrent", "Torrent URL").await {
                        Some(url) => url,
                        None => return,
                    };

                    if let Some(mut job) = handle_backup(&ctx, &command, torrent_url).await {
                        job.job_type = JobType::BackupAll;
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "configure" => {
                    handle_configure(&ctx, &command).await;
                }
                "addapi" => {
                    handle_addapi(&ctx, &command).await;
                }
                "font" => {
                    handle_font(&ctx, &command).await;
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
                    let response_msg = match working_response(&ctx, &command, "...").await {
                        Some(m) => m,
                        None => return,
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
                "merge" => {
                    handle_merge(&ctx, &command).await;
                }
                "source" => {
                    handle_source(&ctx, &command).await;
                }
                "job" => {
                    handle_job(&ctx, &command).await;
                }
                "gitcode" => {
                    let torrent_url = match required_trimmed_option(&ctx, &command, "torrent", "Torrent URL").await {
                        Some(url) => url,
                        None => return,
                    };
                    let preset = resolve_preset(&command, &self.intros);

                    if let Some(job) = handle_gitcode(&ctx, &command, torrent_url, preset).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                "gitsync" => {
                    let response_msg = match working_response(&ctx, &command, "Tüm işlemler kapatılıyor.").await {
                        Some(m) => m,
                        None => return,
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
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "job_id", "Job ID from /probe result")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "index", "File index from probe results")
                        .required(false)
                ),
            CreateCommand::new("backupall")
                .description("Download a torrent and upload every MKV to GDrive")
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
            CreateCommand::new("merge")
                .description("Merge the channel's attached TL and TS subtitles for an episode and upload the release ASS")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "episode", "Episode number (1-based)")
                        .required(true)
                        .min_int_value(1)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "link", "Source link. Falls back to SOURCE.md if omitted.")
                        .required(false)
                ),
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
            CreateCommand::new("addapi")
                .description("Write or update an API token in the toolchain env file")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "key_name", "Env key name (for example `forgejo_api_key`)")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "token", "Token value to write")
                        .required(true)
                ),
            CreateCommand::new("font")
                .description("Download a font zip and extract it to this server's fontconfig directory")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Attachment, "file", "A .zip archive of fonts")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "link", "HTTP(S) link to a .zip archive of fonts")
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
