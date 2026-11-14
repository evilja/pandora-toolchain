use super::*;

use pandora_toolchain::pnworker::caramel::{CaramelJobRequest, VerifySettings};
use pandora_toolchain::pnworker::core::Concat;
use pandora_toolchain::pnworker::server_effects::{load_server_settings, preset_from_name};

// `/caramel-analysis source:<link>` downloads one video the way `/subs` does — a pack is listed
// and the requester picks a file in chat, armed by the caller with `Job::pick_file_first` — and
// runs Caramel Analysis on it instead of encoding it. The estimate is anchored at the `preset`
// option, or at the server's own `/edit` preset when that is not given.
pub async fn handle_caramel(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) -> Option<Job> {
    let source = required_trimmed_option(ctx, command, "source", "Source link").await?;
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
                    list.split(|c: char| c == ',' || c.is_whitespace())
                        .filter_map(|value| value.trim().parse::<f32>().ok())
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
        sampling: Default::default(),
    });
    Some(job)
}
