use serenity::{
    Client,
    all::{ActivityData, ChannelType, CommandOptionType, ComponentInteraction, ComponentInteractionDataKind, Context, CreateEmbed, CreateMessage, EditInteractionResponse, EditMessage, GatewayIntents, Interaction, Message, OnlineStatus, Ready},
    builder::{CreateActionRow, CreateCommand, CreateCommandOption, CreateInteractionResponse, CreateInteractionResponseMessage, CreateSelectMenu, CreateSelectMenuKind, CreateSelectMenuOption, EditChannel},
    prelude::*,
};
use pandora_toolchain::libpnp2p::nyaaise::nyaaise;
use pandora_toolchain::pnworker::core::{HalfJob, Job, JobClass, JobType, Preset};
use pandora_toolchain::pnworker::util::{CliParam, IntrosConfig, PathValue, ToolResult, run_tool};
use pandora_toolchain::pnworker::tools::PNASS_JOB;
use pandora_toolchain::pnworker::tools::PNASS_MERGE;
use pandora_toolchain::pnworker::tools::PNASS_MERGE_TL_ONLY;
use pandora_toolchain::pnworker::tools::PNASS_SPLIT_SIGNS;
use pandora_toolchain::libpnenv::{
    core::{add_env, get_pandora_env, get_perm, remove_env, upsert_env},
    standard::{ENV_PATH, ENV_SEP, TOKEN, CLIENT_ID, CLIENT_SECRET, REFRESH_TOKEN, PARENTID, DOODSTREAM, LULU, VOESX, ABYSS, ANIMECIX},
};
use pandora_toolchain::libpnmal::{fetch_anime, AnimeMeta, AnimeKind};
use pandora_toolchain::libpnforgejo::{Forgejo, base64_encode, base64_encode_bytes};
use pandora_toolchain::libpnanisub::{AniSub, DEFAULT_FPS};
use pandora_toolchain::pnworker::core::pn_worker;
use pandora_toolchain::libpnenv::standard::{PNASS, ANISUB, API_PORT};
use pandora_toolchain::libkagami::core::{SubstationAlpha, find_fonts_with_roots};
use pandora_toolchain::libpnprotocol::core::Protocol;
use tokio::sync::mpsc::{channel, Sender, Receiver};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use regex::Regex;
use reqwest;

#[path = "../helpers/pndc.rs"]
mod pndc_helpers;
use pndc_helpers::*;
#[allow(dead_code)]
#[path = "../helpers/handlers/mod.rs"]
mod handlers;
use handlers::*;

pub struct Handler {
    pub tx: Sender<JobClass>,
    pub intros: IntrosConfig,
}

const ALL_LEVELS: &[&str] = &[
    "witch.pandora",
    "upper.pandora",
    "admin.pandora",
    "fansubber.pandora",
    "authorize.pandora",
];

fn level_rank(name: &str) -> u8 {
    match name {
        "witch.pandora" => 4,
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

const COMMAND_RANKS_PATH: &str = "DB/config/global/environment/command_ranks.pandora";

const DEFAULT_COMMAND_RANKS: &[(&str, u8)] = &[
    ("encode", 0),
    ("pancode", 0),
    ("probe", 0),
    ("backup", 0),
    ("backupall", 0),
    ("gitcode", 0),
    ("smartcode", 0),
    ("merge", 0),
    ("release", 0),
    ("source", 0),
    ("get", 0),
    ("job", 0),
    ("!enc", 0),
    ("!encode", 0),
    ("attach", 1),
    ("init", 1),
    ("detach", 1),
    ("font", 1),
    ("!ts", 1),
    ("destruct", 2),
    ("hearts", 2),
    ("configure", 2),
    ("edit", 2),
    ("readmebase", 2),
    ("addapi", 2),
    ("addtranslation", 2),
    ("gettranslation", 2),
    ("addtranslationall", 2),
    ("gettranslationall", 2),
    ("auth", 2),
    ("remove", 2),
    ("!ban", 2),
    ("!some", 2),
    ("gitsync", 3),
    ("gentoken", 3),
    ("lstoken", 3),
    ("rmtoken", 3),
    ("lsauth", 3),
    ("acixconfirm", 4),
    ("acixtemplate", 4),
    ("changerank", 4),
    ("fontcheck", 4),
];

fn public_command(part: &str) -> bool {
    matches!(part, "help" | "providers")
}

fn parse_command_ranks(contents: &str) -> HashMap<String, u8> {
    let mut ranks = HashMap::new();
    for line in contents.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((key, value)) = line.split_once(ENV_SEP) {
            if let Ok(rank) = value.trim().parse::<u8>() {
                if rank <= 4 {
                    ranks.insert(key.trim().to_string(), rank);
                }
            }
        }
    }
    ranks
}

fn write_command_ranks(ranks: &HashMap<String, u8>) -> Result<(), String> {
    if let Some(parent) = Path::new(COMMAND_RANKS_PATH).parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut out = String::new();
    let mut written = HashSet::new();
    for (name, _) in DEFAULT_COMMAND_RANKS {
        if let Some(rank) = ranks.get(*name) {
            out.push_str(&format!("{}{}{}\n", name, ENV_SEP, rank));
            written.insert((*name).to_string());
        }
    }
    let mut extra = ranks.keys()
        .filter(|name| !written.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    extra.sort();
    for name in extra {
        if let Some(rank) = ranks.get(&name) {
            out.push_str(&format!("{}{}{}\n", name, ENV_SEP, rank));
        }
    }
    std::fs::write(COMMAND_RANKS_PATH, out).map_err(|e| e.to_string())
}

fn ensure_command_ranks_file() -> HashMap<String, u8> {
    let existing = std::fs::read_to_string(COMMAND_RANKS_PATH).unwrap_or_default();
    let mut ranks = parse_command_ranks(&existing);
    let mut changed = existing.is_empty();
    for (name, rank) in DEFAULT_COMMAND_RANKS {
        if !ranks.contains_key(*name) {
            ranks.insert((*name).to_string(), *rank);
            changed = true;
        }
    }
    if changed {
        if let Err(e) = write_command_ranks(&ranks) {
            eprintln!("Failed to write command rank file: {}", e);
        }
    }
    ranks
}

fn set_command_rank(name: &str, rank: u8) -> Result<(), String> {
    if rank > 4 {
        return Err("rank must be between 0 and 4".to_string());
    }
    if !DEFAULT_COMMAND_RANKS.iter().any(|(cmd, _)| *cmd == name) {
        return Err(format!("unknown ranked command `{}`", name));
    }
    let mut ranks = ensure_command_ranks_file();
    ranks.insert(name.to_string(), rank);
    write_command_ranks(&ranks)
}

fn min_rank_for_command(part: &str) -> u8 {
    ensure_command_ranks_file().get(part).copied().unwrap_or(u8::MAX)
}

fn is_authorized(part: &str, id: u64) -> bool {
    if public_command(part) { return true; }
    let min_rank = min_rank_for_command(part);
    if min_rank == u8::MAX { return false; }
    has_level_at_least(id, min_rank)
}

struct HelpCommand {
    name: &'static str,
    summary: &'static str,
    usage: &'static str,
    details: &'static str,
}

fn help_catalog() -> &'static [HelpCommand] {
    &[
        HelpCommand {
            name: "help",
            summary: "Show command help.",
            usage: "/help",
            details: "Opens this private command guide. Pick a command from the menu to see its required inputs and workflow notes.",
        },
        HelpCommand {
            name: "providers",
            summary: "Show attached provider APIs.",
            usage: "/providers",
            details: "Shows built-in download and encode support plus the currently configured upload, distribution, and persistence providers for this server.",
        },
        HelpCommand {
            name: "encode",
            summary: "Encode a torrent or Google Drive source with an ASS subtitle.",
            usage: "/encode torrent:<link> subtitle:<ass> [preset] [concat]",
            details: "Accepts torrent URLs, magnet links, and Google Drive links. The attached subtitle must be ASS. Optional presets control encoder mode; concat selects an intro group.",
        },
        HelpCommand {
            name: "probe",
            summary: "Inspect a torrent and list selectable files.",
            usage: "/probe torrent:<link>",
            details: "Downloads and probes a torrent or magnet link, then returns file indexes. Use the resulting job id and index with /pancode or /backup. Google Drive links are not supported here.",
        },
        HelpCommand {
            name: "pancode",
            summary: "Encode one file from a previous /probe job.",
            usage: "/pancode job_id:<probe_job> index:<file_index> subtitle:<ass> [preset] [concat]",
            details: "Uses the torrent data saved by /probe and encodes the selected file with the provided ASS subtitle.",
        },
        HelpCommand {
            name: "backup",
            summary: "Upload a downloaded source to Drive without release encoding.",
            usage: "/backup torrent:<link> or /backup job_id:<probe_job> index:<file_index>",
            details: "Can download a direct torrent/magnet/Google Drive source, or reuse a probed torrent file when job_id and index are supplied.",
        },
        HelpCommand {
            name: "backupall",
            summary: "Upload every MKV from a torrent to Drive.",
            usage: "/backupall torrent:<link>",
            details: "Downloads the torrent or magnet link and backs up all MKV outputs instead of selecting a single file.",
        },
        HelpCommand {
            name: "gitcode",
            summary: "Encode with a subtitle fetched from a URL.",
            usage: "/gitcode torrent:<link> subtitle_url:<url> [preset] [concat]",
            details: "Fetches the ASS file from a URL. GitHub blob links are rewritten to raw GitHub links automatically.",
        },
        HelpCommand {
            name: "smartcode",
            summary: "Merge attached repo subtitles and encode an episode.",
            usage: "/smartcode episode:<n> [link] [preset] [concat]",
            details: "Requires this channel to be attached to an anime repo. Reads TL and TS files for the episode, merges them, then encodes using the source link or SOURCE.md.",
        },
        HelpCommand {
            name: "merge",
            summary: "Merge TL and TS subtitles for an attached episode.",
            usage: "/merge episode:<n> [link]",
            details: "Requires an attached anime repo. Produces and uploads the release ASS for the episode without starting an encode.",
        },
        HelpCommand {
            name: "release",
            summary: "Upload release fonts for an attached episode.",
            usage: "/release episode:<n>",
            details: "Requires an attached anime repo and an existing release ASS. Reads the release ASS font list and uploads only fonts.zip for that episode folder.",
        },
        HelpCommand {
            name: "source",
            summary: "Write SOURCE.md for an attached episode folder.",
            usage: "/source episode:<n> link:<source_link>",
            details: "Stores the episode source link in the attached Forgejo repo. Source links can be torrent URLs, magnet links, or Google Drive links.",
        },
        HelpCommand {
            name: "job",
            summary: "Upload one episode work file to the attached repo.",
            usage: "/job type:<TL|TLC|TS> episode:<n> subtitle:<ass_or_zip> [commit]",
            details: "Requires a channel attachment. Normalizes the uploaded ASS or root-level ASS zip, then uploads it under the selected job type.",
        },
        HelpCommand {
            name: "get",
            summary: "Get a download link for an episode work file.",
            usage: "/get type:<Translation|Typeset> episode:<n>",
            details: "Returns a repo download link for the requested attached episode file.",
        },
        HelpCommand {
            name: "hearts",
            summary: "Show worker health.",
            usage: "/hearts",
            details: "Reports shrine worker liveness, heartbeat age, and reboot counts.",
        },
        HelpCommand {
            name: "gitsync",
            summary: "Fast-forward the bot repo and restart workers.",
            usage: "/gitsync",
            details: "Runs the configured git sync workflow, archives active work, stops the shrine, and exits for restart.",
        },
        HelpCommand {
            name: "configure",
            summary: "Configure server language, Forgejo, and Google Drive credentials.",
            usage: "/configure language:<EN|TR|JP> [forgejo] [api_key] [gdrive_client_id] [gdrive_client_secret] [gdrive_refresh_token] [gdrive_folder_id] [wrapstyle]",
            details: "Writes server metadata. Run this before /init if the server needs a Forgejo org/base or per-guild Google Drive upload credentials configured. wrapstyle controls ASS WrapStyle normalization; default dont_touch leaves existing subtitles unchanged.",
        },
        HelpCommand {
            name: "edit",
            summary: "Edit individual server metadata fields, leaving the rest untouched.",
            usage: "/edit [language] [forgejo] [api_key] [gdrive_client_id] [gdrive_client_secret] [gdrive_refresh_token] [gdrive_folder_id] [wrapstyle] [announcement_channel]",
            details: "Like /configure but every field is optional — omitted fields keep their current value. Pass `-` to clear a text field. wrapstyle can be dont_touch or 0-3. Set announcement_channel:true to point announcements at the current channel. Requires the server to already be configured.",
        },
        HelpCommand {
            name: "addapi",
            summary: "Write or update a toolchain environment token.",
            usage: "/addapi key_name:<name> token:<value>",
            details: "Updates the global pntools environment file with the provided token value.",
        },
        HelpCommand {
            name: "gettranslation",
            summary: "Read a Pandora localization entry.",
            usage: "/gettranslation language:<en|tr|jp> key:<MESSAGE_KEY>",
            details: "Shows the current text and argument count for one localization key. Language files live at DB/config/en.toml, tr.toml, and jp.toml.",
        },
        HelpCommand {
            name: "addtranslation",
            summary: "Add or update a Pandora localization entry.",
            usage: "/addtranslation language:<en|tr|jp> key:<MESSAGE_KEY> text:<translation> [args]",
            details: "Updates one translation. Existing keys keep args unless provided; new keys infer args from `{}`.",
        },
        HelpCommand {
            name: "gettranslationall",
            summary: "Download a full Pandora localization TOML.",
            usage: "/gettranslationall language:<en|tr|jp>",
            details: "Uploads the selected language file as a TOML attachment.",
        },
        HelpCommand {
            name: "addtranslationall",
            summary: "Replace a full Pandora localization TOML.",
            usage: "/addtranslationall language:<en|tr|jp> file:<toml>",
            details: "Validates and replaces the selected language file from a TOML attachment.",
        },
        HelpCommand {
            name: "gentoken",
            summary: "Generate a new API bearer token.",
            usage: "/gentoken [label:<note>] [local:<true|false>]",
            details: "Mints a random bearer token for the HTTP API and appends it to the token file. With local enabled, jobs submitted with the token use this server's Google Drive credentials when available, falling back to global credentials. The token is shown once, privately. Upper only.",
        },
        HelpCommand {
            name: "acixconfirm",
            summary: "Publish a finished encode to AnimeciX.",
            usage: "/acixconfirm job_id:<id>",
            details: "Confirms the pending AnimeciX publish for an uploaded job and pushes it to AnimeciX (the multishare upload).",
        },
        HelpCommand {
            name: "acixtemplate",
            summary: "Set this channel's AnimeciX fansub id.",
            usage: "/acixtemplate template:<id>",
            details: "Stores the AnimeciX fansub template id (e.g. AkiraSubs=50, SomeSubs=218) on this channel so smartcode publishes are attributed correctly.",
        },
        HelpCommand {
            name: "font",
            summary: "Install a font zip for this server.",
            usage: "/font [file:<zip>] [link:<zip_url>]",
            details: "Accepts either an attached zip or an HTTP(S) zip link, extracts fonts to this server's fontconfig directory, and installs them into the Linux font folder when running on Linux.",
        },
        HelpCommand {
            name: "fontcheck",
            summary: "Count usable unique fonts in the DB fontconfig directories.",
            usage: "/fontcheck",
            details: "Scans DB/fontconfig/global and DB/fontconfig/<server_id>, counts font files and extracts unique usable font names from their name tables.",
        },
        HelpCommand {
            name: "readmebase",
            summary: "Set the server README template.",
            usage: "/readmebase file:<base.md>",
            details: "Stores base.md for repo bootstrapping. /init and /attach can use it when creating or updating README.md.",
        },
        HelpCommand {
            name: "auth",
            summary: "Authorize a user for a permission level.",
            usage: "/auth user_id:<discord_id> [level]",
            details: "Adds a user id to an allowlist. If level is omitted, authorize.pandora is used.",
        },
        HelpCommand {
            name: "remove",
            summary: "Remove a user from a permission level.",
            usage: "/remove user_id:<discord_id> level:<allowlist>",
            details: "Removes a user id from the chosen allowlist.",
        },
        HelpCommand {
            name: "attach",
            summary: "Attach this channel to an existing Forgejo anime repo.",
            usage: "/attach mal:<mal_url> repo:<forgejo_repo> [season] [tl] [tlc] [ts] [qc]",
            details: "Fetches MAL metadata, writes channel metadata, and bootstraps episode folders plus repo helper files.",
        },
        HelpCommand {
            name: "init",
            summary: "Create and attach a new Forgejo repo for an anime.",
            usage: "/init mal:<mal_url> [season] [tl] [tlc] [ts] [qc]",
            details: "Uses the configured Forgejo org, creates a public repo from MAL metadata, bootstraps folders, and attaches this channel.",
        },
        HelpCommand {
            name: "destruct",
            summary: "Delete the attached Forgejo repo and detach this channel.",
            usage: "/destruct",
            details: "Deletes the repo configured for this channel and removes the channel attachment.",
        },
        HelpCommand {
            name: "detach",
            summary: "Detach this channel without deleting the repo.",
            usage: "/detach",
            details: "Removes this channel's anime attachment metadata. The Forgejo repo is left untouched.",
        },
        HelpCommand {
            name: "lstoken",
            summary: "List API bearer tokens.",
            usage: "/lstoken [page]",
            details: "Lists stored API tokens by first and last characters, label, and local binding state.",
        },
        HelpCommand {
            name: "rmtoken",
            summary: "Remove API bearer tokens by label or token mask.",
            usage: "/rmtoken [label:<label>] [token:<abc...xyz>]",
            details: "Removes every token whose stored label exactly matches the supplied label, or one token whose displayed mask matches token.",
        },
        HelpCommand {
            name: "lsauth",
            summary: "List authorized users in one rank.",
            usage: "/lsauth level:<rank>",
            details: "Lists users from the selected permission file as Discord mentions.",
        },
        HelpCommand {
            name: "changerank",
            summary: "Edit a command's required rank.",
            usage: "/changerank command:<name> rank:<0-4>",
            details: "Updates the command rank file for a known command. It cannot change its own rank.",
        },
    ]
}

fn user_help_commands(user_id: u64) -> Vec<&'static HelpCommand> {
    help_catalog().iter()
        .filter(|cmd| public_command(cmd.name) || has_level_at_least(user_id, min_rank_for_command(cmd.name)))
        .collect()
}

fn help_command(name: &str) -> Option<&'static HelpCommand> {
    help_catalog().iter().find(|cmd| cmd.name == name)
}

fn help_rank_label(rank: u8) -> &'static str {
    match rank {
        0 => "Authorize",
        1 => "Fansubber",
        2 => "Admin",
        3 => "Upper",
        4 => "Witch",
        _ => "Unknown",
    }
}

fn help_components(user_id: u64, selected: Option<&str>) -> Vec<CreateActionRow> {
    let commands = user_help_commands(user_id);
    let total_chunks = (commands.len() + 24) / 25;
    commands.chunks(25).enumerate()
        .map(|(idx, chunk)| {
            let options = chunk.iter()
                .map(|cmd| {
                    let option = CreateSelectMenuOption::new(format!("/{}", cmd.name), cmd.name)
                        .description(cmd.summary);
                    if Some(cmd.name) == selected {
                        option.default_selection(true)
                    } else {
                        option
                    }
                })
                .collect();
            let placeholder = if total_chunks > 1 {
                format!("Choose a command ({}/{})", idx + 1, total_chunks)
            } else {
                "Choose a command".to_string()
            };
            CreateActionRow::SelectMenu(
                CreateSelectMenu::new(
                    format!("pnhelp:{}:{}", user_id, idx),
                    CreateSelectMenuKind::String { options },
                )
                    .placeholder(placeholder)
                    .min_values(1)
                    .max_values(1)
            )
        })
        .collect()
}

fn help_overview_embed(user_id: u64) -> CreateEmbed {
    let commands = user_help_commands(user_id);
    let command_list = commands.iter()
        .map(|cmd| format!("`/{}`", cmd.name))
        .collect::<Vec<_>>()
        .join(" ");
    CreateEmbed::new()
        .title("Pandora command help")
        .description("Select a command below to see usage, required inputs, and workflow notes.")
        .field("Available commands", command_list, false)
}

fn help_detail_embed(cmd: &HelpCommand) -> CreateEmbed {
    let access = if public_command(cmd.name) { "Everyone" } else { help_rank_label(min_rank_for_command(cmd.name)) };
    CreateEmbed::new()
        .title(format!("/{}", cmd.name))
        .description(cmd.summary)
        .field("Usage", format!("`{}`", cmd.usage), false)
        .field("Access", access, true)
        .field("Details", cmd.details, false)
}

async fn handle_help_command(ctx: &Context, command: &serenity::all::CommandInteraction) {
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .embed(help_overview_embed(command.user.id.get()))
            .components(help_components(command.user.id.get(), None))
            .ephemeral(true)
    )).await.ok();
}

async fn handle_help_component(ctx: &Context, component: &ComponentInteraction) {
    let Some(owner) = component.data.custom_id.strip_prefix("pnhelp:") else {
        return;
    };
    let owner_id = owner.split(':').next().unwrap_or(owner).parse::<u64>().unwrap_or(0);
    if owner_id != component.user.id.get() {
        component.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("Run `/help` to open your own command guide.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    let selected = match &component.data.kind {
        ComponentInteractionDataKind::StringSelect { values } => values.first().map(String::as_str),
        _ => None,
    };
    let Some(name) = selected else {
        component.create_response(ctx, CreateInteractionResponse::Acknowledge).await.ok();
        return;
    };
    let Some(cmd) = help_command(name) else {
        component.create_response(ctx, CreateInteractionResponse::Acknowledge).await.ok();
        return;
    };
    if !public_command(cmd.name) && !has_level_at_least(component.user.id.get(), min_rank_for_command(cmd.name)) {
        component.create_response(ctx, CreateInteractionResponse::Message(
            CreateInteractionResponseMessage::new()
                .content("You do not have access to that command.")
                .ephemeral(true)
        )).await.ok();
        return;
    }

    component.create_response(ctx, CreateInteractionResponse::UpdateMessage(
        CreateInteractionResponseMessage::new()
            .embed(help_detail_embed(cmd))
            .components(help_components(component.user.id.get(), Some(cmd.name)))
    )).await.ok();
}

fn server_wrap_style(server_id: u64) -> String {
    let path = format!("DB/config/{}/meta.pandora", server_id);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.lines().nth(8).map(String::from))
        .filter(|s| matches!(s.as_str(), "0" | "1" | "2" | "3"))
        .unwrap_or_else(|| "keep".to_string())
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
    #[serde(default)]
    acix_template: Option<i64>,
}

fn default_season() -> u16 { 1 }

fn default_credit() -> String { "---".to_string() }

fn perm_path(name: &str) -> String {
    format!("DB/config/global/perms/{}", name)
}

async fn migrate_pandora_files() {
    let _ = tokio::fs::create_dir_all("DB/config/global/perms").await;
    let _ = tokio::fs::create_dir_all("DB/config/global/environment").await;

    for name in ["authorize.pandora", "fansubber.pandora", "admin.pandora", "upper.pandora", "witch.pandora"] {
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
            if let Some(t) = m.acix_template {
                out.push_str(&format!("acix_template = {}\n", t));
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

fn channel_kind_label(kind: ChannelType) -> Option<&'static str> {
    match kind {
        ChannelType::Text => Some("Text"),
        ChannelType::News => Some("Announcement"),
        ChannelType::Forum => Some("Forum"),
        ChannelType::PublicThread => Some("Thread"),
        ChannelType::PrivateThread => Some("Private Thread"),
        ChannelType::NewsThread => Some("Announcement Thread"),
        _ => None,
    }
}

async fn sync_guild_channels(ctx: &Context, guild_id: u64) {
    let gid = serenity::all::GuildId::new(guild_id);
    let mut list: Vec<serde_json::Value> = Vec::new();

    if let Ok(channels) = gid.channels(&ctx.http).await {
        for (id, ch) in channels {
            if let Some(label) = channel_kind_label(ch.kind) {
                list.push(serde_json::json!({ "id": id.get().to_string(), "name": ch.name, "kind": label }));
            }
        }
    }
    if let Ok(active) = gid.get_active_threads(&ctx.http).await {
        for ch in active.threads {
            if let Some(label) = channel_kind_label(ch.kind) {
                list.push(serde_json::json!({ "id": ch.id.get().to_string(), "name": ch.name, "kind": label }));
            }
        }
    }

    list.sort_by(|a, b| {
        a["name"].as_str().unwrap_or("").to_lowercase()
            .cmp(&b["name"].as_str().unwrap_or("").to_lowercase())
    });

    let dir = format!("DB/config/{}", guild_id);
    if let Err(e) = tokio::fs::create_dir_all(&dir).await {
        eprintln!("[channels] create_dir {} failed: {}", dir, e);
        return;
    }
    let path = format!("{}/channels.json", dir);
    match serde_json::to_string(&list) {
        Ok(s) => {
            if let Err(e) = tokio::fs::write(&path, s).await {
                eprintln!("[channels] write {} failed: {}", path, e);
            }
        }
        Err(e) => eprintln!("[channels] serialize failed: {}", e),
    }
}

async fn auto_detach_channel(server_id: u64, channel_id: u64) {
    let meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() && meta.repo_url.as_deref().map_or(true, str::is_empty) {
        return;
    }
    let path = meta_path(server_id, channel_id);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => {
            println!(
                "[detach] auto-detached deleted channel {} in server {} (anime: {})",
                channel_id, server_id, meta.name.unwrap_or_default()
            );
            if let Some(parent) = path.parent() {
                let _ = tokio::fs::remove_dir(parent).await;
            }
        }
        Err(e) => eprintln!(
            "[detach] failed to remove meta for deleted channel {} in server {}: {}",
            channel_id, server_id, e
        ),
    }
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
    let has_gitignore = existing.iter().any(|n| n.eq_ignore_ascii_case(".gitignore"));
    if !has_gitignore {
        fg.create_file(owner_repo, ".gitignore", &base64_encode("*.mkv\n"), "bootstrap gitignore").await?;
        created.push(".gitignore".to_string());
    }
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

    async fn cache_ready(&self, ctx: Context, guilds: Vec<serenity::all::GuildId>) {
        for gid in guilds {
            sync_guild_channels(&ctx, gid.get()).await;
        }
    }

    async fn guild_create(&self, ctx: Context, guild: serenity::all::Guild, _is_new: Option<bool>) {
        sync_guild_channels(&ctx, guild.id.get()).await;
    }

    async fn channel_create(&self, ctx: Context, channel: serenity::all::GuildChannel) {
        sync_guild_channels(&ctx, channel.guild_id.get()).await;
    }

    async fn channel_update(&self, ctx: Context, _old: Option<serenity::all::GuildChannel>, new: serenity::all::GuildChannel) {
        sync_guild_channels(&ctx, new.guild_id.get()).await;
    }

    async fn channel_delete(&self, ctx: Context, channel: serenity::all::GuildChannel, _messages: Option<Vec<Message>>) {
        auto_detach_channel(channel.guild_id.get(), channel.id.get()).await;
        sync_guild_channels(&ctx, channel.guild_id.get()).await;
    }

    async fn thread_create(&self, ctx: Context, thread: serenity::all::GuildChannel) {
        sync_guild_channels(&ctx, thread.guild_id.get()).await;
    }

    async fn thread_delete(&self, ctx: Context, thread: serenity::all::PartialGuildChannel, _full_thread_data: Option<serenity::all::GuildChannel>) {
        auto_detach_channel(thread.guild_id.get(), thread.id.get()).await;
        sync_guild_channels(&ctx, thread.guild_id.get()).await;
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
                "help" => {
                    handle_help_command(&ctx, &command).await;
                }
                "providers" => {
                    handle_providers(&ctx, &command).await;
                }
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
                "edit" => {
                    handle_edit(&ctx, &command).await;
                }
                "addapi" => {
                    handle_addapi(&ctx, &command).await;
                }
                "gettranslation" => {
                    handle_gettranslation(&ctx, &command).await;
                }
                "addtranslation" => {
                    handle_addtranslation(&ctx, &command).await;
                }
                "gettranslationall" => {
                    handle_gettranslationall(&ctx, &command).await;
                }
                "addtranslationall" => {
                    handle_addtranslationall(&ctx, &command).await;
                }
                "gentoken" => {
                    handle_gentoken(&ctx, &command).await;
                }
                "lstoken" => {
                    handle_lstoken(&ctx, &command).await;
                }
                "rmtoken" => {
                    handle_rmtoken(&ctx, &command).await;
                }
                "lsauth" => {
                    handle_lsauth(&ctx, &command).await;
                }
                "changerank" => {
                    handle_changerank(&ctx, &command).await;
                }
                "acixconfirm" => {
                    handle_acixconfirm(&ctx, &command).await;
                }
                "acixtemplate" => {
                    handle_acixtemplate(&ctx, &command).await;
                }
                "font" => {
                    handle_font(&ctx, &command).await;
                }
                "fontcheck" => {
                    handle_fontcheck(&ctx, &command).await;
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
                "release" => {
                    handle_release(&ctx, &command).await;
                }
                "source" => {
                    handle_source(&ctx, &command).await;
                }
                "get" => {
                    handle_get(&ctx, &command).await;
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
        } else if let Interaction::Component(component) = interaction {
            if component.data.custom_id.starts_with("pnhelp:") {
                handle_help_component(&ctx, &component).await;
            }
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        println!("{} is connected!", ready.user.name);
        println!("Bot ID: {}", ready.user.id);
        println!("Serving {} guilds", ready.guilds.len());

        ctx.set_presence(Some(ActivityData::custom("Pandora is active.")), OnlineStatus::Online);
        pandora_toolchain::pnworker::presence::set_global_context(ctx.clone());

        let mut concat_option = CreateCommandOption::new(
            CommandOptionType::String,
            "concat",
            "Intro"
        ).required(false);

        for group_name in self.intros.groups.keys() {
            concat_option = concat_option.add_string_choice(group_name, group_name);
        }

        let commands = vec![
            CreateCommand::new("help")
                .description("Open an interactive command guide"),
            CreateCommand::new("providers")
                .description("Show attached provider APIs"),
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
            CreateCommand::new("release")
                .description("Upload fonts.zip for an existing episode release ASS")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "episode", "Episode number (1-based)")
                        .required(true)
                        .min_int_value(1)
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
            CreateCommand::new("get")
                .description("Get the download link for an episode's translation or typeset file")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "type", "File type")
                        .required(true)
                        .add_string_choice("Translation", "Translation")
                        .add_string_choice("Typeset", "Typeset")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "episode", "Episode number (1-based)")
                        .required(true)
                        .min_int_value(1)
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
                .description("Configure this server (language, Forgejo, Google Drive)")
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
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_client_id", "Google Drive OAuth client id. Omit to keep the existing one.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_client_secret", "Google Drive OAuth client secret. Omit to keep the existing one.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_refresh_token", "Google Drive OAuth refresh token. Omit to keep the existing one.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_folder_id", "Google Drive upload folder id. Omit to keep the existing one.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "wrapstyle", "ASS WrapStyle normalization. Default dont_touch.")
                        .required(false)
                        .add_string_choice("dont_touch", "dont_touch")
                        .add_string_choice("0", "0")
                        .add_string_choice("1", "1")
                        .add_string_choice("2", "2")
                        .add_string_choice("3", "3")
                ),
            CreateCommand::new("edit")
                .description("Edit individual server metadata fields, leaving the rest untouched")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "language", "Bot language. Omit to keep the existing one.")
                        .required(false)
                        .add_string_choice("English", "EN")
                        .add_string_choice("Türkçe", "TR")
                        .add_string_choice("日本語", "JP")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "forgejo", "Forgejo base link. Omit to keep, `-` to unset.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "api_key", "Forgejo API token. Omit to keep, `-` to unset.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_client_id", "Google Drive OAuth client id. Omit to keep, `-` to unset.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_client_secret", "Google Drive OAuth client secret. Omit to keep, `-` to unset.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_refresh_token", "Google Drive OAuth refresh token. Omit to keep, `-` to unset.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "gdrive_folder_id", "Google Drive upload folder id. Omit to keep, `-` to unset.")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "wrapstyle", "ASS WrapStyle normalization. Use dont_touch to clear.")
                        .required(false)
                        .add_string_choice("dont_touch", "dont_touch")
                        .add_string_choice("0", "0")
                        .add_string_choice("1", "1")
                        .add_string_choice("2", "2")
                        .add_string_choice("3", "3")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Boolean, "announcement_channel", "Set the announcement channel to this channel.")
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
            CreateCommand::new("gettranslation")
                .description("Read a translation")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "language", "Language")
                        .required(true)
                        .add_string_choice("English", "en")
                        .add_string_choice("Türkçe", "tr")
                        .add_string_choice("日本語", "jp")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "key", "Message key")
                        .required(true)
                ),
            CreateCommand::new("addtranslation")
                .description("Edit a translation")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "language", "Language")
                        .required(true)
                        .add_string_choice("English", "en")
                        .add_string_choice("Türkçe", "tr")
                        .add_string_choice("日本語", "jp")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "key", "Message key")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "text", "Text")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "args", "Placeholder count")
                        .required(false)
                        .min_int_value(0)
                ),
            CreateCommand::new("gettranslationall")
                .description("Download translations")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "language", "Language")
                        .required(true)
                        .add_string_choice("English", "en")
                        .add_string_choice("Türkçe", "tr")
                        .add_string_choice("日本語", "jp")
                ),
            CreateCommand::new("addtranslationall")
                .description("Upload translations")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "language", "Language")
                        .required(true)
                        .add_string_choice("English", "en")
                        .add_string_choice("Türkçe", "tr")
                        .add_string_choice("日本語", "jp")
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Attachment, "file", "TOML file")
                        .required(true)
                ),
            CreateCommand::new("gentoken")
                .description("Generate a new API bearer token (upper only)")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "label", "Optional note stored beside the token")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Boolean, "local", "Bind token to this server for Drive creds and git console access")
                        .required(false)
                ),
            CreateCommand::new("lstoken")
                .description("List API bearer tokens")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "page", "Page number")
                        .required(false)
                        .min_int_value(1)
                ),
            CreateCommand::new("rmtoken")
                .description("Remove API tokens by exact label or displayed mask")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "label", "Exact token label to remove")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "token", "Displayed token mask, for example c79...d03")
                        .required(false)
                ),
            CreateCommand::new("lsauth")
                .description("List authorized users in one rank level")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "level", "Auth level file")
                        .required(true)
                        .add_string_choice("Authorize", "authorize.pandora")
                        .add_string_choice("Fansubber", "fansubber.pandora")
                        .add_string_choice("Admin", "admin.pandora")
                        .add_string_choice("Upper", "upper.pandora")
                        .add_string_choice("Witch", "witch.pandora")
                ),
            CreateCommand::new("changerank")
                .description("Edit a command's required rank")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "command", "Command name without slash")
                        .required(true)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "rank", "Required rank, 0 through 4")
                        .required(true)
                        .min_int_value(0)
                        .max_int_value(4)
                ),
            CreateCommand::new("acixconfirm")
                .description("[BETA-TESTING] Confirm and publish an encode to AnimeciX")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "job_id", "The job id (from the upload message)")
                        .required(true)
                ),
            CreateCommand::new("acixtemplate")
                .description("[BETA-TESTING] Set channel AnimeciX fansub template id")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Integer, "template", "Fansub id (e.g. AkiraSubs=50, SomeSubs=218)")
                        .required(true)
                ),
            CreateCommand::new("font")
                .description("Download a font zip and install it for this server")
                .add_option(
                    CreateCommandOption::new(CommandOptionType::Attachment, "file", "A .zip archive of fonts")
                        .required(false)
                )
                .add_option(
                    CreateCommandOption::new(CommandOptionType::String, "link", "HTTP(S) link to a .zip archive of fonts")
                        .required(false)
                ),
            CreateCommand::new("fontcheck")
                .description("Count usable unique fonts in the DB fontconfig directories"),
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
                        .add_string_choice("Fansubber", "fansubber.pandora")
                        .add_string_choice("Admin", "admin.pandora")
                        .add_string_choice("Upper", "upper.pandora")
                        .add_string_choice("Witch", "witch.pandora")
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
                        .add_string_choice("Fansubber", "fansubber.pandora")
                        .add_string_choice("Admin", "admin.pandora")
                        .add_string_choice("Upper", "upper.pandora")
                        .add_string_choice("Witch", "witch.pandora")
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
    ensure_command_ranks_file();
    pandora_toolchain::libpnbin::ensure_startup_binaries().await;
    match install_persisted_pandora_fonts().await {
        Ok(Some(installed)) => {
            let dirs = installed.dirs.iter()
                .map(|dir| dir.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let cache = if installed.cache_refreshed { "refreshed" } else { "not refreshed" };
            println!("[fonts] installed {} persisted font file(s) to {} (font cache {})", installed.count, dirs, cache);
        }
        Ok(None) => {}
        Err(e) => eprintln!("[fonts] persisted font install failed: {}", e),
    }
    let env = get_pandora_env();
    let (tx, rx): (Sender<JobClass>, Receiver<JobClass>) = channel(5);
    tokio::spawn(pn_worker(rx));
    if let Some(port) = env.get(API_PORT).and_then(|s| s.trim().parse::<u16>().ok()).filter(|p| *p != 0) {
        let api_tx = tx.clone();
        tokio::spawn(async move {
            if let Err(e) = pandora_toolchain::libpnapi::serve(api_tx, port).await {
                eprintln!("[Pandora API] {e}");
            }
        });
    }
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
