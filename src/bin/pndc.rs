use serenity::{
    Client,
    all::{ActivityData, CommandOptionType, Context, CreateMessage, GatewayIntents, Interaction, Message, OnlineStatus, Ready},
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

pub async fn handle_attach_stub(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    label: &str,
) {
    let tmdb_url = command.data.options.iter()
        .find(|opt| opt.name == "tmdb")
        .and_then(|opt| opt.value.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);
    let repo_url = command.data.options.iter()
        .find(|opt| opt.name == "repo")
        .and_then(|opt| opt.value.as_str())
        .filter(|s| !s.is_empty())
        .map(String::from);

    if tmdb_url.is_none() && repo_url.is_none() {
        command.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Error: either `tmdb` or `repo` link is required")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(t) = tmdb_url { parts.push(format!("TMDB: `{}`", t)); }
    if let Some(r) = repo_url { parts.push(format!("Repo: `{}`", r)); }
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("{} registered — {} (stub).", label, parts.join(", ")))
            .ephemeral(true)
    )).await.ok();
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
        Some(u) if u.starts_with("http://") || u.starts_with("https://") => u.trim_end_matches('/').to_string(),
        _ => {
            command.create_response(ctx, CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content("Error: forgejo must be an http(s) URL")
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

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!("Configured server `{}` — language: {}, forgejo: `{}`, announcement channel: <#{}>",
                server_id, language, forgejo, command.channel_id.get()))
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
                    handle_attach_stub(&ctx, &command, "/attach").await;
                }
                "init" => {
                    handle_attach_stub(&ctx, &command, "/init").await;
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
                .description("Attach metadata to a job (TMDB link and/or Forgejo repo link) (stub)")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "tmdb", "TMDB link")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "repo", "Forgejo repo link (e.g. https://git.einzu.fun/owner/repo)")
                        .required(false)
                ),
            CreateCommand::new("init")
                .description("Initialize a job from a TMDB link (stub)")
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
                    CreateCommandOption::new(CommandOptionType::String, "forgejo", "Forgejo base link (e.g. https://git.einzu.fun)")
                        .required(true)
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
