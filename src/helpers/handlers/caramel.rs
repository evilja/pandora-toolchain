use super::*;

use pandora_toolchain::pnworker::caramel::{CaramelJobRequest, VerifySettings};
use pandora_toolchain::pnworker::core::Concat;
use pandora_toolchain::pnworker::server_effects::{load_server_settings, preset_from_name};

// `/caramel-analysis source:<link>` downloads one video the way `/subs` does — a pack is listed
// and the requester picks a file in chat, armed by the caller with `Job::pick_file_first` — and
// runs Caramel Analysis on it instead of encoding it. Every scene's CRF is anchored at the
// `preset` option, or at the server's own `/edit` preset when that is not given.
//
// `/caramel-analysis-encode` (`fast`) is the same, with every scene decided after a fast encode of
// all of it, by a model that reads one; it is refused while this machine has no such model.
pub async fn handle_caramel(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    fast: bool,
) -> Option<Job> {
    let source = required_trimmed_option(ctx, command, "source", "Source link").await?;
    if fast && pandora_toolchain::lib::mpeg::caramel_train::fast_model().is_none() {
        command_error(ctx, command, command_format(command, CARAMEL_NO_FAST_MODEL, &[])).await;
        return None;
    }
    let server_id = command.guild_id.map(|guild| guild.get());
    let preset = match option_trimmed(command, "preset") {
        Some(name) => match preset_from_name(&name, Concat::NONE).and_then(|preset| preset.name()) {
            Some(canonical) => canonical,
            None => {
                command_error(ctx, command, command_format(command, CARAMEL_UNKNOWN_PRESET, &[name])).await;
                return None;
            }
        },
        None => load_server_settings(server_id).preset.name().unwrap_or_else(|| "standard".to_string()),
    };
    let anchor = option_f64(command, "anchor").map(|anchor| anchor as f32);
    let verify = option_bool(command, "verify")
        .unwrap_or(false)
        .then(|| VerifySettings {
            crfs: option_trimmed(command, "verify_crfs")
                .map(|list| {
                    // Four at most: the embed has one line per scene with every CRF on it. The API
                    // takes more, for ground-truth runs nobody reads in an embed.
                    list.split(|c: char| c == ',' || c.is_whitespace())
                        .filter_map(|value| value.trim().parse::<f32>().ok())
                        .take(4)
                        .collect()
                })
                .unwrap_or_default(),
            ..Default::default()
        });

    let response_msg = working_response(ctx, command, "...").await?;
    if let Err(error) = response_msg.react(ctx, '❌').await {
        report_send_failure("cancel reaction", response_msg.channel_id.get(), &error);
    }
    let mut job = Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Caramel,
        response_msg.id.get(),
        nyaaise(&source),
        Vec::new(),
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        server_id,
    );
    job.display_link = Some(display_source_link(&source));
    job.caramel = Some(CaramelJobRequest {
        preset: Some(preset),
        anchor,
        verify,
        scenes: Default::default(),
        fast,
        host: fast,
    });
    Some(job)
}
