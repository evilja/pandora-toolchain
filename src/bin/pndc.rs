use serenity::{
    Client,
    all::{Attachment, Command, CommandOptionType, CommandType, Context, EditMessage, GatewayIntents, Interaction, Message, Ready},
    builder::{CreateCommand, CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage},
    prelude::*,
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
use pandora_toolchain::pnworker::core::{Job, Preset, JobType};
use pandora_toolchain::pnworker::core::pn_worker;

pub struct Handler {
    pub tx: Sender<Job>
}

#[serenity::async_trait]
impl EventHandler for Handler {
    async fn message(&self, context: Context, msg: Message) {
        // Only process messages from the specific user
        if msg.author.id.get() != 944246988575215627 {
            return;
        }

        // Check if message has an attachment (subtitle file)
        if msg.attachments.is_empty() {
            return;
        }

        // Parse command from message content
        let parts: Vec<&str> = msg.content.split_whitespace().collect();

        if parts.is_empty() {
            return;
        }

        match parts[0] {
            "!enc" | "/encode" => {
                // Parse torrent URL (required)
                let torrent_url = if parts.len() > 1 {
                    parts[1].to_string()
                } else {
                    msg.reply(&context, "Error: Torrent URL required").await.ok();
                    return;
                };

                // Parse preset (optional, defaults to pseudolossless)
                let preset_str = parts.get(2).unwrap_or(&"standard");

                // Parse concat value (optional)
                let concat = parts.get(3)
                    .and_then(|s| s.parse::<i16>().ok());

                let preset = match *preset_str {
                    "gpu" => Preset::Gpu(concat),
                    "standard" | "x264" => Preset::Standard(concat),
                    _ => Preset::PseudoLossless(concat),
                };

                self.tx.send(
                    Job::new(
                        msg.author.id.get(),
                        msg.channel_id.get(),
                        JobType::Encode,
                        msg.id.get(),
                        preset,
                        torrent_url,
                        context,
                        msg
                    )
                ).await.unwrap();
            }
            _ => {}
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        if let Interaction::Command(command) = interaction {
            match command.data.name.as_str() {
                "encode" => {
                    // Get options from the command
                    let torrent_url = command.data.options.iter()
                        .find(|opt| opt.name == "torrent")
                        .and_then(|opt| opt.value.as_str())
                        .unwrap_or("");

                    let preset_str = command.data.options.iter()
                        .find(|opt| opt.name == "preset")
                        .and_then(|opt| opt.value.as_str())
                        .unwrap_or("pseudolossless");

                    let concat = command.data.options.iter()
                        .find(|opt| opt.name == "concat")
                        .and_then(|opt| opt.value.as_i64())
                        .map(|v| v as i16);

                    let attachment_id = command.data.options.iter()
                        .find(|opt| opt.name == "subtitle")
                        .and_then(|opt| opt.value.as_attachment_id());

                    println!("{:?}", attachment_id);
                    if torrent_url.is_empty() {
                        let response = CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content("Error: Torrent URL is required")
                                .ephemeral(true)
                        );
                        command.create_response(&ctx.http, response).await.ok();
                        return;
                    }

                    if attachment_id.is_none() {
                        let response = CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content("Error: Subtitle file is required")
                                .ephemeral(true)
                        );
                        command.create_response(&ctx.http, response).await.ok();
                        return;
                    }

                    let preset = match preset_str {
                        "gpu" => Preset::Gpu(concat),
                        "standard" => Preset::Standard(concat),
                        _ => Preset::PseudoLossless(concat),
                    };

                    // Send initial response
                    let response = CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("Merhaba, isteğiniz alındı.")
                    );

                    if let Err(why) = command.create_response(&ctx.http, response).await {
                        println!("Cannot respond to slash command: {}", why);
                        return;
                    }

                    // Get the response message - this includes the attachment!
                    if let Ok(msg) = command.get_response(&ctx.http).await {
                        self.tx.send(
                            Job::new(
                                msg.author.id.get(),
                                msg.channel_id.get(),
                                JobType::Encode,
                                msg.id.get(),
                                preset,
                                torrent_url.to_string(),
                                ctx,  // Not (context, msg)
                                msg
                            )
                        ).await.unwrap();
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
}

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
