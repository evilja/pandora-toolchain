use std::sync::Arc;
use serenity::builder::CreateAttachment;
use serenity::all::{ActivityData, Context, CreateEmbed, EditMessage, Message, OnlineStatus};
use tokio::time::{sleep, Duration};
use crate::pnworker::core::Job;
use crate::pnworker::messages::{
    get_message, MessagePayload, create_job_embed, PREVIEW_ATTACHMENT_MISSING,
    PREVIEW_ATTACHMENT_REJECTED, PREVIEW_DONE, PROBE_ROW, STUDIO_PREVIEW_ATTACHMENT_MISSING,
    STUDIO_PREVIEW_DONE, SUBS_ATTACHMENT_MISSING, SUBS_DONE,
};
use crate::pnworker::presence::{change_presence_job, global_context, Presence};
use crate::pnworker::probe_pages::{probe_page_components, probe_page_count};
use serenity::all::CreateActionRow;

#[derive(Clone)]
pub enum Frontend {
    Discord { ctx: Arc<Context>, msg: Message },
    Web,
    None,
}

impl Frontend {
    pub fn discord(ctx: Context, msg: Message) -> Self {
        Frontend::Discord { ctx: Arc::new(ctx), msg }
    }

    pub async fn update(&mut self, job: &Job, payload: &MessagePayload) {
        match self {
            Frontend::Discord { ctx, msg } => {
                if is_attachment_done(payload) {
                    match preview_done_edit(job, payload).await {
                        Some(edit) => {
                            if msg.edit(&**ctx, edit).await.is_ok() {
                                return;
                            }
                            eprintln!("[Pandora Preview] Discord preview attachment edit failed for {}", job.job_id);
                            let mut display_job = job.clone();
                            display_job.encode_warnings.push(get_message(
                                PREVIEW_ATTACHMENT_REJECTED,
                                &job.lang,
                            ));
                            let embed = create_job_embed(&display_job, payload);
                            let _ = msg.edit(&**ctx, EditMessage::new().content("").embed(embed)).await;
                            return;
                        }
                        None => {
                            let id = if is_studio_preview_done(payload) {
                                STUDIO_PREVIEW_ATTACHMENT_MISSING
                            } else if is_subs_done(payload) {
                                SUBS_ATTACHMENT_MISSING
                            } else {
                                PREVIEW_ATTACHMENT_MISSING
                            };
                            let mut display_job = job.clone();
                            display_job
                                .encode_warnings
                                .push(get_message(id, &job.lang));
                            let embed = create_job_embed(&display_job, payload);
                            let _ = msg.edit(&**ctx, EditMessage::new().content("").embed(embed)).await;
                            return;
                        }
                    }
                }
                let edit = EditMessage::new()
                    .content("")
                    .embed(create_job_embed(job, payload))
                    .components(probe_page_buttons(job, payload));
                if let Err(error) = msg.edit(&**ctx, edit).await {
                    eprintln!("[Pandora Frontend] Discord update failed for job {}: {}", job.job_id, error);
                }
            }
            Frontend::Web => {
                if is_attachment_done(payload) {
                    eprintln!("[Pandora Preview] preview attachments are Discord-only for job {}", job.job_id);
                }
            }
            Frontend::None => {}
        }
    }

    // A batch with no output page reports every episode separately, so each child job needs its own
    // message in the channel the batch was requested from.
    pub async fn spawn_child_message(&self, content: &str) -> Frontend {
        match self {
            Frontend::Discord { ctx, msg } => match msg
                .channel_id
                .say(&ctx.http, content.to_string())
                .await
            {
                Ok(message) => Frontend::Discord {
                    ctx: ctx.clone(),
                    msg: message,
                },
                Err(error) => {
                    eprintln!("[Pandora Batch] child message could not be created: {error}");
                    Frontend::None
                }
            },
            Frontend::Web | Frontend::None => Frontend::None,
        }
    }

    pub async fn set_text(&mut self, text: &str) {
        match self {
            Frontend::Discord { ctx, msg } => {
                let _ = msg.edit(&**ctx, EditMessage::new().content(text.to_string())).await;
            }
            Frontend::Web => {}
            Frontend::None => {}
        }
    }

    pub async fn set_embed(&mut self, embed: CreateEmbed) {
        match self {
            Frontend::Discord { ctx, msg } => {
                let _ = msg.edit(&**ctx, EditMessage::new().content("").embed(embed)).await;
            }
            Frontend::Web => {}
            Frontend::None => {}
        }
    }

    pub async fn delete(&self) {
        match self {
            Frontend::Discord { ctx, msg } => {
                let _ = msg.delete(&**ctx).await;
            }
            Frontend::Web => {}
            Frontend::None => {}
        }
    }

    pub async fn mark_failed(&self) {
        match self {
            Frontend::Discord { ctx, msg } => {
                let _ = msg.react(&**ctx, '☠').await;
            }
            Frontend::Web => {}
            Frontend::None => {}
        }
    }

    pub async fn ghost_ping(&self, author: u64) {
        match self {
            Frontend::Discord { ctx, msg } => {
                if let Ok(ping) = msg.channel_id.say(&ctx.http, format!("<@{}>", author)).await {
                    sleep(Duration::from_millis(750)).await;
                    let _ = ping.delete(&ctx.http).await;
                }
            }
            Frontend::Web => {}
            Frontend::None => {}
        }
    }

    pub async fn set_presence(&self, presence: Presence) {
        match self {
            Frontend::Discord { ctx, .. } => change_presence_job(ctx, presence).await,
            Frontend::Web => {
                if let Some(ctx) = global_context() {
                    change_presence_job(ctx, presence).await;
                }
            }
            Frontend::None => {}
        }
    }

    pub fn notify_recompiling(&self) {
        match self {
            Frontend::Discord { ctx, .. } => {
                ctx.set_presence(Some(ActivityData::custom("Recompiling Pandora.")), OnlineStatus::Idle);
            }
            Frontend::Web => {
                if let Some(ctx) = global_context() {
                    ctx.set_presence(Some(ActivityData::custom("Recompiling Pandora.")), OnlineStatus::Idle);
                }
            }
            Frontend::None => {}
        }
    }
}

fn is_preview_done(payload: &MessagePayload) -> bool {
    matches!(payload, MessagePayload::Progress(id, _) if *id == PREVIEW_DONE)
}

fn is_studio_preview_done(payload: &MessagePayload) -> bool {
    matches!(payload, MessagePayload::Progress(id, _) if *id == STUDIO_PREVIEW_DONE)
}

fn is_subs_done(payload: &MessagePayload) -> bool {
    matches!(payload, MessagePayload::Progress(id, _) if *id == SUBS_DONE)
}

fn is_attachment_done(payload: &MessagePayload) -> bool {
    is_preview_done(payload) || is_studio_preview_done(payload) || is_subs_done(payload)
}

// Probe file lists longer than one embed field get prev/next buttons; every other payload sends an
// empty row set so a later edit of the same message clears buttons left over from the list.
fn probe_page_buttons(job: &Job, payload: &MessagePayload) -> Vec<CreateActionRow> {
    let MessagePayload::Progress(id, args) = payload else {
        return Vec::new();
    };
    if *id != PROBE_ROW {
        return Vec::new();
    }
    let pages = probe_page_count(args.first().map(String::as_str).unwrap_or(""));
    probe_page_components(job.job_id, 1, pages)
}

async fn preview_done_edit(job: &Job, payload: &MessagePayload) -> Option<EditMessage> {
    let MessagePayload::Progress(id, args) = payload else {
        return None;
    };
    if *id != PREVIEW_DONE && *id != STUDIO_PREVIEW_DONE && *id != SUBS_DONE {
        return None;
    }
    // Extraction always answers with exactly one attachment: the single track, or
    // the archive the worker bundled the tracks into.
    if *id == SUBS_DONE {
        let path = args.get(1)?;
        return match CreateAttachment::path(path).await {
            Ok(attachment) => Some(
                EditMessage::new()
                    .content("")
                    .embed(create_job_embed(job, payload))
                    .new_attachment(attachment),
            ),
            Err(e) => {
                eprintln!("[Pandora Subs] failed to attach `{}`: {}", path, e);
                None
            }
        };
    }
    if *id == STUDIO_PREVIEW_DONE {
        let path = args.first()?;
        return match CreateAttachment::path(path).await {
            Ok(mut attachment) => {
                attachment.filename = "studio-preview.mp4".to_string();
                Some(EditMessage::new().content("").embed(create_job_embed(job, payload)).new_attachment(attachment))
            }
            Err(e) => {
                eprintln!("[Pandora Studio] failed to attach preview `{}`: {}", path, e);
                None
            }
        };
    }
    if args.len() == 2 {
        let path = &args[1];
        return match CreateAttachment::path(path).await {
            Ok(mut attachment) => {
                attachment.filename = "preview.png".to_string();
                Some(
                    EditMessage::new()
                        .content("")
                        .embed(create_job_embed(job, payload).image("attachment://preview.png"))
                        .new_attachment(attachment),
                )
            }
            Err(e) => {
                eprintln!(
                    "[Pandora Preview] failed to attach merged preview from `{}`: {}",
                    path, e
                );
                None
            }
        };
    }
    let mut edit = EditMessage::new()
        .content("")
        .embed(create_job_embed(job, payload));
    let mut added = 0usize;
    let mut idx = 1usize;
    while idx + 1 < args.len() {
        let label = &args[idx];
        let path = &args[idx + 1];
        match CreateAttachment::path(path).await {
            Ok(attachment) => {
                edit = edit.new_attachment(attachment);
                added += 1;
            }
            Err(e) => {
                eprintln!(
                    "[Pandora Preview] failed to attach preview `{}` from `{}`: {}",
                    label, path, e
                );
            }
        }
        idx += 2;
    }
    if added == 0 {
        None
    } else {
        Some(edit)
    }
}
