use serenity::{
    Client,
    all::{ActivityData, CommandOptionType, Context, CreateMessage, EditMessage, GatewayIntents, Interaction, Message, OnlineStatus, Ready},
    builder::{CreateCommand, CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage},
    prelude::*,
};
use pandora_toolchain::libpnp2p::nyaaise::nyaaise;
use pandora_toolchain::pnworker::core::{HalfJob, Job, JobClass, JobType, Preset};
use pandora_toolchain::pnworker::util::IntrosConfig;
use pandora_toolchain::libpnenv::{
    core::{add_env, get_env, get_perm},
    standard::TOKEN,
};
use pandora_toolchain::libpntmdb::{fetch_anime, AnimeMeta, AnimeKind};
use pandora_toolchain::libpnforgejo::{Forgejo, base64_encode};
use pandora_toolchain::pnworker::core::pn_worker;
use tokio::sync::mpsc::{channel, Sender, Receiver};
use regex::Regex;
use reqwest;

pub struct Handler {
    pub tx: Sender<JobClass>,
    pub intros: IntrosConfig,
}

fn is_authorized(part: &str, id: u64) -> bool {
    let class = match part {
        "!enc" | "!encode" => "authorize.pandora",
        "!ban" => "admin.pandora",
        "!authorize" | "!auth" => "admin.pandora",
        "encode" | "pancode" | "probe" | "backup" | "scrape" | "gitcode" => "authorize.pandora",
        "attach" | "init" => "upper.pandora",
        "!some" => "admin.pandora",
        "gitsync" | "hearts" | "configure" => "admin.pandora",
        _ => return false,
    };
    let allowed = get_perm(class.to_string());

    !allowed.is_empty() && allowed.contains(&id.to_string())
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
    tmdb_id: Option<u64>,
    kind: Option<String>,
    name: Option<String>,
    slug: Option<String>,
    episode_count: Option<u32>,
    repo_url: Option<String>,
    season: Option<u32>,
}

fn meta_to_toml(m: &ChannelMeta) -> String {
    match (&m.kind, m.tmdb_id) {
        (Some(k), Some(id)) => {
            let season_line = match m.season {
                Some(s) => format!("season = {}\n", s),
                None => String::new(),
            };
            format!("tmdb_id = {}\nkind = \"{}\"\nname = \"{}\"\nslug = \"{}\"\nepisode_count = {}\nrepo_url = \"{}\"\n{}",
                id, k, m.name.as_deref().unwrap_or(""), m.slug.as_deref().unwrap_or(""),
                m.episode_count.unwrap_or(0), m.repo_url.as_deref().unwrap_or(""), season_line)
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
    create_root_readme: bool,
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
    if create_root_readme || !has_readme {
        let readme = base_md.unwrap_or_default();
        let b64 = base64_encode(&readme);
        fg.create_file(owner_repo, "README.md", &b64, "bootstrap root readme").await?;
        created.push("README.md".to_string());
    }

    Ok(created)
}

async fn read_server_meta(server_id: u64) -> Result<(String, String), String> {
    let path = format!("DB/config/{}/meta.pandora", server_id);
    let s = tokio::fs::read_to_string(&path).await.map_err(|e| e.to_string())?;
    let mut lines = s.lines();
    let lang = lines.next().unwrap_or("tr").to_string();
    let forgejo = lines.next().unwrap_or("").to_string();
    Ok((lang, forgejo))
}

fn kind_label(k: &AnimeKind) -> &'static str {
    match k {
        AnimeKind::Movie => "Movie",
        AnimeKind::MultiEpisode => "MultiEpisode",
    }
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

    let tmdb_url = match command.data.options.iter()
        .find(|opt| opt.name == "tmdb")
        .and_then(|opt| opt.value.as_str())
        .filter(|s| !s.is_empty())
    {
        Some(u) => u.to_string(),
        None => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(format!("Error: `tmdb` is required for {}", label))
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

    let existing = read_channel_meta(server_id, channel_id);

    let (_lang, forgejo_base) = match read_server_meta(server_id).await {
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

    let meta = match fetch_anime(&tmdb_url).await {
        Ok(m) => m,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("TMDB fetch failed: {}", e))).await;
            return;
        }
    };

    if let Some(eid) = existing.tmdb_id {
        if eid != meta.tmdb_id || existing.season != meta.season {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!(
                "Channel is already attached to `{}`. Use a different channel to attach a new anime.",
                existing.name.unwrap_or_default()
            ))).await;
            return;
        }
    }

    let fg = match Forgejo::from_env(forgejo_base.clone()) {
        Ok(f) => f,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Forgejo init failed: {}", e))).await;
            return;
        }
    };

    let (owner_repo, repo_url) = if is_init {
        let or = format!("{}/{}", fg.org, meta.slug);
        let url = match fg.create_repo(&meta.slug).await {
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

    let base_md = tokio::fs::read_to_string(format!("DB/config/{}/base.md", server_id)).await.ok();

    let created = match bootstrap_repo(&fg, &owner_repo, &meta, base_md, is_init, existing_root).await {
        Ok(v) => v,
        Err(e) => {
            let _ = response_msg.edit(ctx, EditMessage::new().content(format!("Bootstrap failed: {}", e))).await;
            return;
        }
    };

    let new_meta = ChannelMeta {
        tmdb_id: Some(meta.tmdb_id),
        kind: Some(kind_label(&meta.kind).to_string()),
        name: Some(meta.name.clone()),
        slug: Some(meta.slug.clone()),
        episode_count: Some(meta.episode_count),
        repo_url: Some(repo_url.clone()),
        season: meta.season,
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
    let body = format!(
        "**{}** — attached to this channel.\nName: `{}`\nSlug: `{}`\nKind: `{}`\nEpisodes: `{}`\nRepo: <{}>\nCreated/updated: {}",
        label, meta.name, meta.slug, kind_label(&meta.kind), meta.episode_count, repo_url, created_list
    );
    let _ = response_msg.edit(ctx, EditMessage::new().content(body)).await;
}

pub async fn handle_init(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, true).await;
}

pub async fn handle_attach(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, false).await;
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

    let body = format!("{}\n{}\n{}\n", language, forgejo, command.channel_id.get());
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
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Configured server `{}` — language: {}, forgejo: {}, announcement channel: <#{}>",
                server_id, language, forgejo_display, command.channel_id.get()))
            .ephemeral(true)
    )).await.ok();
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
            "!authorize" | "!auth" => {
                let mut to_auth = parts[1].to_string();
                match add_env("authorize.pandora", &mut to_auth) {
                    true  => { msg.reply(context, format!("Merhaba, Pandora'ya hoş geldiniz.\nYetkilendirilen kullanıcı: <@{}>", parts[1])).await.unwrap(); }
                    false => { msg.reply(context, "Merhaba, Pandora'ya hoş geldiniz.\nKullanıcı yetkilendirilemedi.").await.unwrap(); }
                }
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
                .description("Attach a TMDB anime to this channel and bootstrap an existing Forgejo repo")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "tmdb", "TMDB link")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "repo", "Forgejo repo link (e.g. https://git.einzu.fun/owner/repo)")
                        .required(true)
                ),
            CreateCommand::new("init")
                .description("Attach a TMDB anime to this channel and create a new Forgejo repo for it")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "tmdb", "TMDB link")
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
                .description("Configure this server (language and Forgejo base link)")
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
    let env = get_env("env.pandora".into());
    let (tx, rx): (Sender<JobClass>, Receiver<JobClass>) = channel(5);
    tokio::spawn(pn_worker(rx));
    let intros = IntrosConfig::load();
    println!("{:?}", intros);
    pandora_toolchain::pnworker::messages::init_language_files();
    let mut discord = Client::builder(env[TOKEN].clone(), GatewayIntents::all())
        .event_handler(Handler { tx, intros })
        .await
        .unwrap();

    if let Err(why) = discord.start().await {
        println!("{}", why);
    }
}
