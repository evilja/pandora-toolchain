use std::sync::Arc;
use serenity::builder::CreateAttachment;
use serenity::all::{ActivityData, Context, CreateEmbed, EditMessage, Message, OnlineStatus};
use tokio::time::{sleep, Duration};
use crate::pnworker::core::Job;
use crate::pnworker::messages::{MessagePayload, create_job_embed, PREVIEW_DONE};
use crate::pnworker::presence::{change_presence_job, global_context, Presence};

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
                if let Some(edit) = preview_done_edit(job, payload).await {
                    if msg.edit(&**ctx, edit).await.is_ok() {
                        return;
                    }
                    eprintln!("[Pandora Preview] Discord preview attachment edit failed for {}", job.job_id);
                }
                let _ = msg.edit(&**ctx, EditMessage::new().content("").embed(create_job_embed(job, payload))).await;
            }
            Frontend::Web => {
                if is_preview_done(payload) {
                    eprintln!("[Pandora Preview] preview attachments are Discord-only for job {}", job.job_id);
                }
            }
            Frontend::None => {}
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

async fn preview_done_edit(job: &Job, payload: &MessagePayload) -> Option<EditMessage> {
    let MessagePayload::Progress(id, args) = payload else {
        return None;
    };
    if *id != PREVIEW_DONE {
        return None;
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
