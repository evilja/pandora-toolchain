use super::*;

use pandora_toolchain::pnworker::probe_pages::{
    is_probe_list_value, parse_probe_component_id, probe_page_body, probe_page_components,
    probe_page_count,
};
use serenity::all::{ComponentInteraction, Embed};
use serenity::builder::CreateEmbedFooter;

pub async fn handle_probe(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    torrent_url: String,
) -> Option<Job> {
    let response_msg = working_response(ctx, command, "...").await?;

    Some(Job::new(
        command.user.id.get(),
        command.channel_id.get(),
        response_msg.id.get(),
        JobType::Probe,
        response_msg.id.get(),
        nyaaise(&torrent_url),
        vec![],                // no attachment
        ctx.clone(),
        response_msg,
        read_lang(command.guild_id),
        command.guild_id.map(|g| g.get()),
    ))
}

// The probe message only ever holds one page of the file list, so a page button re-reads the whole
// list from the job's stored progress and rewrites the embed it was clicked on. Nothing about the
// job is kept in memory for this, which is what lets it survive a `pndc` restart mid-probe.
pub async fn handle_probe_component(ctx: &Context, component: &ComponentInteraction) {
    let Some((job_id, page)) = parse_probe_component_id(&component.data.custom_id) else {
        acknowledge(ctx, component).await;
        return;
    };
    let lang = read_lang(component.guild_id);
    let Some(files) = probe_file_list(job_id).await else {
        component
            .create_response(
                ctx,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(get_message(PROBE_PAGE_EXPIRED, &lang))
                        .ephemeral(true),
                ),
            )
            .await
            .ok();
        return;
    };
    let total = probe_page_count(&files);
    let page = page.clamp(1, total.max(1));
    let Some(embed) = component.message.embeds.first() else {
        acknowledge(ctx, component).await;
        return;
    };
    let Some(rebuilt) = swap_probe_list(embed, &probe_page_body(&files, page, &lang)) else {
        acknowledge(ctx, component).await;
        return;
    };
    component
        .create_response(
            ctx,
            CreateInteractionResponse::UpdateMessage(
                CreateInteractionResponseMessage::new()
                    .embed(rebuilt)
                    .components(probe_page_components(job_id, page, total)),
            ),
        )
        .await
        .ok();
}

async fn acknowledge(ctx: &Context, component: &ComponentInteraction) {
    component
        .create_response(ctx, CreateInteractionResponse::Acknowledge)
        .await
        .ok();
}

async fn probe_file_list(job_id: u64) -> Option<String> {
    let db = pandora_toolchain::lib::db::core::JobDb::new().await.ok()?;
    let row = db.get_job(job_id).await.ok()??;
    let progress: serde_json::Value = serde_json::from_str(row.progress.as_deref()?).ok()?;
    if progress.get("type").and_then(|value| value.as_str()) != Some("probe") {
        return None;
    }
    progress
        .get("files")
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

// Rebuilt rather than composed from scratch: the status/job id/worker/source fields belong to the
// worker's own render, and the button handler has no `Job` to reproduce them from.
fn swap_probe_list(embed: &Embed, body: &str) -> Option<CreateEmbed> {
    if !embed.fields.iter().any(|field| is_probe_list_value(&field.value)) {
        return None;
    }
    let mut rebuilt = CreateEmbed::new();
    if let Some(title) = &embed.title {
        rebuilt = rebuilt.title(title);
    }
    if let Some(description) = &embed.description {
        rebuilt = rebuilt.description(description);
    }
    if let Some(colour) = embed.colour {
        rebuilt = rebuilt.colour(colour);
    }
    for field in &embed.fields {
        let value = if is_probe_list_value(&field.value) {
            body.to_string()
        } else {
            field.value.clone()
        };
        rebuilt = rebuilt.field(&field.name, value, field.inline);
    }
    if let Some(image) = &embed.image {
        rebuilt = rebuilt.image(&image.url);
    }
    if let Some(footer) = &embed.footer {
        rebuilt = rebuilt.footer(CreateEmbedFooter::new(&footer.text));
    }
    Some(rebuilt.timestamp(serenity::model::Timestamp::now()))
}
