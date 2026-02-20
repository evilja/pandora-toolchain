use serenity::{
    Client,
    all::{CommandOptionType, CreateMessage, Context, GatewayIntents, Interaction, Message, Ready},
    builder::{CreateCommand, CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage},
    prelude::*,
};
use pandora_toolchain::libpnp2p::nyaaise::nyaaise;
use pandora_toolchain::pnworker::core::{HalfJob, Job, JobClass, JobType, Preset};
use tokio::{self,
    sync::mpsc::{
        channel,
        Sender,
        Receiver,
    }
};
use pandora_toolchain::libpnenv::{
    core::{add_env, get_env, get_perm},
    standard::TOKEN
};
use pandora_toolchain::pnworker::core::pn_worker;

pub struct Handler {
    pub tx: Sender<JobClass>
}

fn is_authorized(part: &str, id: u64) -> bool {
    let class = match part {
        "!enc" | "/encode" => {
            "authorize.pandora"
        }
        "!authorize" | "!auth" => {
            "admin.pandora"
        }
        _ => {
            return false
        }
    };
    get_perm(class.to_string()).contains(&id.to_string())
}
// pndc.rs - add to the existing file



// For !enc message command
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

    let response_msg = match msg.channel_id.send_message(
        context,
        CreateMessage::new().content("...")
    ).await {
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
        nyaaise(&torrent_url).0,
        attachment_bytes,
        context.clone(),
        response_msg,
    ))
}

// For /encode slash command
pub async fn handle_interaction(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
    preset: Preset,
) -> Option<Job> {
    let attachment_id = command.data.options.iter()
        .find(|opt| opt.name == "subtitle")
        .and_then(|opt| opt.value.as_attachment_id());

    let attachment_bytes = match attachment_id
        .and_then(|id| command.data.resolved.attachments.get(&id))
    {
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
        nyaaise(&torrent_url).0,
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
            "!authorize" | "!auth" => {
                let mut to_auth = parts[1].to_string();
                match add_env("authorize.pandora", &mut to_auth) {
                    true => { msg.reply(context, format!("Merhaba, Pandora Toolchain'e hoş geldiniz, Myisha-P.\nYetkilendirilen kullanıcı: <@{}>", parts[1])).await.unwrap(); }
                    false => { msg.reply(context, "Merhaba, Pandora Toolchain'e hoş geldiniz, Myisha-P.\nKullanıcı yetkilendirilemedi.").await.unwrap(); }
                }
            }
            _ => {}
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
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

                    let concat = command.data.options.iter()
                        .find(|opt| opt.name == "concat")
                        .and_then(|opt| opt.value.as_i64())
                        .map(|v| v as i16);

                    let preset = match command.data.options.iter()
                        .find(|opt| opt.name == "preset")
                        .and_then(|opt| opt.value.as_str())
                        .unwrap_or("standard")
                    {
                        "gpu" => Preset::Gpu(concat),
                        "standard" => Preset::Standard(concat),
                        _ => Preset::PseudoLossless(concat),
                    };

                    if let Some(job) = handle_interaction(&ctx, &command, torrent_url, preset).await {
                        self.tx.send(JobClass::Job(job)).await.unwrap();
                    }
                }
                _ => {}
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        println!("Bot ID: {}", ready.user.id);
        println!("Serving {} guilds", ready.guilds.len());

        // Register slash commands
        let commands = vec![
            CreateCommand::new("encode")
                .description("Encode a video with subtitle")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "torrent",
                        "Torrent URL or magnet link"
                    )
                    .required(true)
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Attachment,
                        "subtitle",
                        "ASS subtitle file"
                    )
                    .required(true)
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "preset",
                        "Encoding preset"
                    )
                    .required(false)
                    .add_string_choice("Pseudo Lossless", "pseudolossless")
                    .add_string_choice("Standard x264", "standard")
                    .add_string_choice("GPU", "gpu")
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::Integer,
                        "concat",
                        "Concat video ID (optional)"
                    )
                    .required(false)
                )
        ];

        // Register per-guild (instant)
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
                    self.tx.send(JobClass::HalfJob(HalfJob::new_cancel(user_id.get(), reaction.channel_id.get(), reaction.message_id.get()))).await.ok();
                }
            }
        }
    }
}

#[tokio::main]
async fn main () {
    let env = get_env("env.pandora".into());
    let (tx, rx): (Sender<JobClass>, Receiver<JobClass>) = channel(5);

    tokio::spawn(pn_worker(rx));

    let mut discord = Client::builder(env[TOKEN].clone(), GatewayIntents::all())
        .event_handler(Handler { tx: tx })
        .await
        .unwrap();

    if let Err(why) = discord.start().await {
        println!("{}", why);
    }
}

/*use serenity::{
    Client,
    all::{GatewayIntents},
};
use tokio::{self,
    sync::mpsc::{
        channel,
        Sender,
        Receiver,
    }
};
use pandora_toolchain::libpnenv::{
    core::get_env,
    standard::TOKEN
};
use pandora_toolchain::pnworker::core::{Job, Handler};
use pandora_toolchain::pnworker::core::pn_worker;


#[tokio::main]
async fn main () {
    let env = get_env("env.pandora".into());
    let (tx, rx): (Sender<Job>, Receiver<Job>) = channel(5);

    tokio::spawn(pn_worker(rx));

    let mut discord = Client::builder(env[TOKEN].clone(), GatewayIntents::all())
        .event_handler(Handler { tx: tx })
        .await
        .unwrap();

    if let Err(why) = discord.start().await {
        println!("{}", why);
    }
}
*/
