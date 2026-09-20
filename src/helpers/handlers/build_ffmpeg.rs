use super::*;

use pandora_toolchain::lib::bin::{
    build_native_ffmpeg, native_build_blocker, native_ffmpeg_build_running, NativeBuildBlocker,
    FFMPEG_BUILD_STEPS,
};

// `/build-ffmpeg`: compile ffmpeg for this machine's CPU into `DB/bin`, the same build
// `pndc --build-ffmpeg` and `ffmpeg_build|pntools|native` run. It takes long enough that the
// interaction token would expire before it ends, so the reply is the public "working" message
// and a task edits that message when the build finishes — the same shape a job's message has.
// While it runs the same message names the step the script has reached and the minutes spent, so
// a quarter of an hour of compiling does not look like a command that hung.
//
// A container with no compiler is refused before the script runs. The script would refuse it too,
// but with package-manager lines that mean nothing inside an image, when the useful answers are
// "the image already ships a native ffmpeg" or "rebuild it with the toolchain".
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

    if let Some(blocker) = native_build_blocker() {
        let text = match blocker {
            NativeBuildBlocker::NoCompilerNativeInUse { built_at, tuning, record } => {
                command_format(command, BUILD_FFMPEG_NO_COMPILER_NATIVE, &[built_at, tuning, record])
            }
            NativeBuildBlocker::NoCompilerNativeShadowed => command_format(command, BUILD_FFMPEG_NO_COMPILER_SHADOWED, &[]),
            NativeBuildBlocker::NoCompiler => command_format(command, BUILD_FFMPEG_NO_COMPILER, &[]),
        };
        command_error(ctx, command, text).await;
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
        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
        let build = build_native_ffmpeg(clean, Some(progress_tx));
        tokio::pin!(build);
        // Milestones can arrive several to a second (every component "already built"), and the
        // minutes change with no milestone at all, so the message is redrawn on a timer from
        // whatever is latest rather than once per line — well inside Discord's edit rate limit.
        let started = std::time::Instant::now();
        let mut redraw = tokio::time::interval(std::time::Duration::from_secs(10));
        let mut latest: Option<(u8, String)> = None;
        let mut shown = String::new();
        let result = loop {
            tokio::select! {
                result = &mut build => break result,
                Some(progress) = progress_rx.recv() => {
                    latest = Some((progress.step, progress.line));
                }
                _ = redraw.tick() => {
                    let Some((step, line)) = &latest else { continue };
                    let text = format_message(BUILD_FFMPEG_PROGRESS, &lang, &[
                        step.to_string(),
                        FFMPEG_BUILD_STEPS.to_string(),
                        progress_line(line),
                        (started.elapsed().as_secs() / 60).to_string(),
                        FFMPEG_BUILD_LOG_PATH.to_string(),
                    ]);
                    if text != shown {
                        if let Err(error) = response_msg.edit(&ctx, EditMessage::new().content(&text)).await {
                            report_send_failure("build-ffmpeg progress", response_msg.channel_id.get(), &error);
                        }
                        shown = text;
                    }
                }
            }
        };
        let text = match result {
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

// A milestone as it goes into the message's code span: the configure line runs to a thousand
// characters, and a backtick in it would end the span early.
fn progress_line(line: &str) -> String {
    let line = line.replace('`', "'");
    if line.chars().count() <= 160 {
        return line;
    }
    format!("{}…", line.chars().take(159).collect::<String>())
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
