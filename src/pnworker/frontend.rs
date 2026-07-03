use std::sync::Arc;
use serenity::all::{ActivityData, Context, EditMessage, Message, OnlineStatus};
use crate::pnworker::core::Job;
use crate::pnworker::messages::{MessagePayload, create_job_embed};
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
                let _ = msg.edit(&**ctx, EditMessage::new().content("").embed(create_job_embed(job, payload))).await;
            }
            Frontend::Web => {}
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
