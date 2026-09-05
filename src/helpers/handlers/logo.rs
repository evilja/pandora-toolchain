use super::*;

use pandora_toolchain::lib::mpeg::logo::{
    LogoPeriod, LogoPlacement, LogoPosition, MAX_LOGO_MARGIN, MAX_LOGO_WIDTH_PERCENT,
    MIN_LOGO_WIDTH_PERCENT, ServerLogo, detect_logo_format, format_duration_seconds,
};
use pandora_toolchain::pnworker::server_effects::{
    clear_server_logo, load_server_logo, save_server_logo,
};

// Discord's own attachment ceiling is well above this; the limit exists because the picture is
// copied into every job directory and travels in full inside every leased job's spec.
const MAX_LOGO_BYTES: usize = 4 * 1024 * 1024;

// The image watermark, beside `/touchwatermark`'s ASS one. The two are separate commands because
// they are separate things: an ASS watermark is text libass draws into the subtitle stream and can
// be styled and timed per event, and a logo is a picture the encoder composites over every frame.
// A server may configure either, both, or neither.
pub async fn handle_touchlogo(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let server_id = match command_server_id(ctx, command, "/touchlogo").await {
        Some(id) => id,
        None => return,
    };

    if option_bool(command, "clear").unwrap_or(false) {
        match clear_server_logo(server_id) {
            Ok(true) => {
                logo_saved(ctx, command, "Removed this server's image watermark. Jobs already queued keep the logo they were created with.").await;
            }
            Ok(false) => {
                command_error(ctx, command, "Error: this server has no image watermark to clear.").await;
            }
            Err(e) => {
                command_error(ctx, command, format!("Failed to clear the image watermark: {}", e)).await;
            }
        }
        return;
    }

    // Reading first is what lets the placement options be used on their own: an operator moving an
    // existing logo to the other corner should not have to re-upload the picture.
    let existing = load_server_logo(server_id);
    let placement = match requested_placement(command, existing.as_ref().map(|logo| &logo.placement)) {
        Ok(placement) => placement,
        Err(e) => {
            command_error(ctx, command, e).await;
            return;
        }
    };

    let attachment = option_attachment(command, "image");
    if attachment.is_none() && existing.is_none() {
        command_error(
            ctx,
            command,
            "Error: `image` is required until this server has an image watermark. Upload one, then the placement options can be used on their own.",
        )
        .await;
        return;
    }

    if command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(CreateInteractionResponseMessage::new().ephemeral(true)),
        )
        .await
        .is_err()
    {
        return;
    }

    let logo = match attachment {
        None => ServerLogo {
            placement,
            ..existing.expect("an absent image was refused above without an existing logo")
        },
        Some(attachment) => {
            if attachment.size as usize > MAX_LOGO_BYTES {
                logo_response(ctx, command, format!(
                    "Error: `image` is {} bytes; the limit is {} bytes because the picture is copied into every job and travels with every leased one.",
                    attachment.size, MAX_LOGO_BYTES
                )).await;
                return;
            }
            let bytes = match attachment.download().await {
                Ok(bytes) => bytes,
                Err(e) => {
                    logo_response(ctx, command, format!("Failed to download the image: {}", e)).await;
                    return;
                }
            };
            if bytes.len() > MAX_LOGO_BYTES {
                logo_response(ctx, command, format!(
                    "Error: `image` is {} bytes; the limit is {} bytes.",
                    bytes.len(), MAX_LOGO_BYTES
                )).await;
                return;
            }
            // The signature decides, not the file name: ffmpeg opens the stored file by its
            // extension, so a PNG uploaded as `.jpg` would fail at encode time with nothing
            // connecting the failure back to this upload.
            let Some(extension) = detect_logo_format(&bytes) else {
                logo_response(
                    ctx,
                    command,
                    "Error: `image` must be a PNG, JPEG, or WebP. PNG is the one of the three that carries transparency, which is almost always what a watermark wants.",
                )
                .await;
                return;
            };
            ServerLogo {
                bytes,
                extension: extension.to_string(),
                placement,
            }
        }
    };

    let bytes = logo.bytes.len();
    let placement = logo.placement.clone();
    let saved = tokio::task::spawn_blocking(move || save_server_logo(server_id, &logo)).await;
    match saved {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            logo_response(ctx, command, format!("Failed to save the image watermark: {}", e)).await;
            return;
        }
        Err(e) => {
            logo_response(ctx, command, format!("Failed to save the image watermark: {}", e)).await;
            return;
        }
    }

    let size = match placement.width_percent {
        Some(percent) => format!("{percent}% of the frame width"),
        None => "its own pixel size".to_string(),
    };
    let cadence = match placement.period {
        Some(period) => format!(
            "shown for {} out of every {}, fading in and out at each end",
            format_duration_seconds(period.visible_seconds),
            format_duration_seconds(period.every_seconds),
        ),
        None => "on every frame".to_string(),
    };
    logo_saved(ctx, command, format!(
        "Saved this server's image watermark: {} bytes, drawn at **{}** with a {}px margin, {}% opacity, at {}, {}.\nIt is burned into future Encode, Pancode, and batch jobs. Jobs already queued keep the logo they were created with.",
        bytes, placement.position.name(), placement.margin, placement.opacity, size, cadence,
    )).await;
}

// The placement this invocation asks for: each option that was given, over the one already stored,
// over the default. Values are checked here rather than clamped, because an operator who typed 500
// meant something and should be told it is not a percentage rather than silently given 50.
fn requested_placement(
    command: &serenity::all::CommandInteraction,
    existing: Option<&LogoPlacement>,
) -> Result<LogoPlacement, String> {
    let current = existing.cloned().unwrap_or_default();
    let position = match option_str(command, "position") {
        None => current.position,
        Some(name) => LogoPosition::from_name(name)
            .ok_or_else(|| format!("Error: `{}` is not a position.", name))?,
    };
    let margin = match option_i64(command, "margin") {
        None => current.margin,
        Some(value) if (0..=MAX_LOGO_MARGIN as i64).contains(&value) => value as u32,
        Some(value) => {
            return Err(format!(
                "Error: `margin` is {value}px; it must be between 0 and {MAX_LOGO_MARGIN}."
            ));
        }
    };
    let opacity = match option_i64(command, "opacity") {
        None => current.opacity,
        Some(value) if (1..=100).contains(&value) => value as u8,
        Some(value) => {
            return Err(format!(
                "Error: `opacity` is {value}%; it must be between 1 and 100."
            ));
        }
    };
    // `0` clears the percentage rather than setting one, which is how the image is put back to its
    // own pixel size without re-uploading it.
    let width_percent = match option_i64(command, "width") {
        None => current.width_percent,
        Some(0) => None,
        Some(value)
            if (MIN_LOGO_WIDTH_PERCENT as i64..=MAX_LOGO_WIDTH_PERCENT as i64).contains(&value) =>
        {
            Some(value as u8)
        }
        Some(value) => {
            return Err(format!(
                "Error: `width` is {value}%; it must be between {MIN_LOGO_WIDTH_PERCENT} and {MAX_LOGO_WIDTH_PERCENT}, or 0 to use the image's own size."
            ));
        }
    };
    // `off` puts the logo back on every frame rather than setting a cadence, the way `width` takes
    // `0` — a server dropping the period should not have to re-upload the picture to do it.
    let period = match option_str(command, "period") {
        None => current.period,
        Some(value)
            if matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "off" | "none" | "always" | "0"
            ) =>
        {
            None
        }
        Some(value) => Some(LogoPeriod::parse(value).map_err(|e| format!("Error: {e}"))?),
    };
    Ok(LogoPlacement {
        position,
        margin,
        opacity,
        width_percent,
        period,
    })
}

// The command defers before it downloads, so the answer is an edit; `clear` never defers and gets a
// fresh message. One helper for both, because either way the operator has to see the outcome.
async fn logo_saved(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    let embed = success_embed(command, COMMAND_UPDATED).description(content.into());
    if command
        .edit_response(
            ctx,
            EditInteractionResponse::new().content("").embed(embed.clone()),
        )
        .await
        .is_ok()
    {
        return;
    }
    command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new().embed(embed).ephemeral(true),
            ),
        )
        .await
        .ok();
}

async fn logo_response(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    content: impl Into<String>,
) {
    command
        .edit_response(ctx, EditInteractionResponse::new().content(content.into()))
        .await
        .ok();
}
