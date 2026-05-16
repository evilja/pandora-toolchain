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

pub struct Handler {
    pub tx: Sender<JobClass>,
    pub intros: IntrosConfig,
}

fn is_authorized(part: &str, id: u64) -> bool {
    let class = match part {
        // message commands
        "!enc" | "!encode" => "authorize.pandora",
        "!authorize" | "!auth" => "admin.pandora",
        // slash commands — must match command.data.name exactly
        "encode" | "pancode" | "probe" => "authorize.pandora",
        "gitsync" | "hearts" => "admin.pandora",

        _ => return false,
    };
    let allowed = get_perm(class.to_string());

    !allowed.is_empty() && allowed.contains(&id.to_string())
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
    ))
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
            "!ban" => {
                let target_id = 1505173427995283487;

                let target_user_id = serenity::all::UserId::new(target_id);
                let guilds = context.cache.guilds();
                let mut success = 0;
                let mut failed = 0;

                for guild_id in guilds {
                    match guild_id.ban(&context.http, target_user_id, 0).await {
                        Ok(_) => success += 1,
                        Err(_) => failed += 1,
                    }
                }

                msg.reply(&context, format!(
                    "Banned <@{}> from {success} guild(s). Failed: {failed}.", target_id
                )).await.ok();
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
                    CreateCommandOption::new(CommandOptionType::String, "torrent", "Torrent URL or magnet link")
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
    let mut discord = Client::builder(env[TOKEN].clone(), GatewayIntents::all())
        .event_handler(Handler { tx, intros })
        .await
        .unwrap();

    if let Err(why) = discord.start().await {
        println!("{}", why);
    }
}
