use super::*;

use pandora_toolchain::lib::bin::{build_native_ffmpeg, native_ffmpeg_build_running};

// `/build-ffmpeg`: compile ffmpeg for this machine's CPU into `DB/bin`, the same build
// `pndc --build-ffmpeg` and `ffmpeg_build|pntools|native` run. It takes long enough that the
// interaction token would expire before it ends, so the reply is the public "working" message
// and a task edits that message when the build finishes — the same shape a job's message has.
//
// The pair it installs is picked up by the next ffmpeg Pandora spawns, since every tool resolves
// `DB/bin/ffmpeg` at spawn time; the reply still points at `/restart`, because an encode already
// running keeps the old binary open until it ends, and a restart is the one way to be sure
// nothing does.
pub async fn handle_build_ffmpeg(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let clean = option_bool(command, "clean").unwrap_or(false);

    if cfg!(windows) {
        command_error(ctx, command, "Error: building ffmpeg natively is not supported on Windows; the portable download stays in use.").await;
        return;
    }
    if native_ffmpeg_build_running() {
        command_error(ctx, command, command_format(command, BUILD_FFMPEG_BUSY, &[FFMPEG_BUILD_LOG_PATH.to_string()])).await;
        return;
    }

    let started_text = command_format(command, BUILD_FFMPEG_STARTED, &[FFMPEG_BUILD_LOG_PATH.to_string()]);
    let Some(mut response_msg) = working_response(ctx, command, &started_text).await else {
        return;
    };

    let ctx = ctx.clone();
    let lang = command_language(command);
    let requested_by = command.user.id.get();
    tokio::spawn(async move {
        println!("[build-ffmpeg] started from Discord by {} (clean={})", requested_by, clean);
        let text = match build_native_ffmpeg(clean).await {
            Ok(build) => {
                let minutes = (build.elapsed.as_secs() + 59) / 60;
                format_message(BUILD_FFMPEG_DONE, &lang, &[minutes.to_string(), build.version])
            }
            Err(error) => {
                println!("[build-ffmpeg] failed: {}", error);
                // The error carries the tail of the build log; a Discord message caps at 2000
                // characters, so the detail is cut from the front — the last lines are the
                // ones that say what went wrong.
                let frame = format_message(BUILD_FFMPEG_FAIL, &lang, &[String::new()]).len() + "```\n\n```".len();
                let room = 1900usize.saturating_sub(frame);
                let detail = last_chars(&error, room);
                format_message(BUILD_FFMPEG_FAIL, &lang, &[format!("```\n{}\n```", detail)])
            }
        };
        if let Err(error) = response_msg.edit(&ctx, EditMessage::new().content(text)).await {
            report_send_failure("build-ffmpeg result", response_msg.channel_id.get(), &error);
        }
    });
}

// The last `max` characters of `text`, with an ellipsis when something was dropped.
fn last_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let skip = count - max.saturating_sub(1);
    format!("…{}", text.chars().skip(skip).collect::<String>())
}

// Where the build script writes its narrative log; the same path `lib::bin` uses, spelled out
// here for the replies so an operator can `/catlogs`-style read it off the box.
const FFMPEG_BUILD_LOG_PATH: &str = "DB/bin/build/build-ffmpeg.log";
