use super::*;
use pandora_toolchain::lib::http::acix::refresh_fansub_templates;
use pandora_toolchain::lib::http::anizm::refresh_publishing_catalog;
use pandora_toolchain::lib::http::openanime::refresh_fansubs;
use pandora_toolchain::pnworker::server_config::FansubSite;

// The persisted directories refresh themselves every 12 hours, which is too slow to pick up a
// fansub that was created minutes ago. Every site is refreshed inline here rather than in the
// background, so the reply reports what each provider actually returned; a site that fails keeps
// its previous copy, exactly as a background refresh would.
pub async fn handle_refreshcache(ctx: &Context, command: &serenity::all::CommandInteraction) {
    if command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .is_err()
    {
        return;
    }

    let results = vec![
        (
            FansubSite::AnimeciX,
            refresh_fansub_templates().await.map(|templates| templates.len()),
        ),
        (
            FansubSite::OpenAnime,
            refresh_fansubs().await.map(|fansubs| fansubs.len()),
        ),
        (
            FansubSite::Anizm,
            refresh_publishing_catalog()
                .await
                .map(|catalog| catalog.fansubs.len()),
        ),
    ];

    let refreshed = results.iter().filter(|(_, result)| result.is_ok()).count();
    let mut lines = vec![command_format(
        command,
        REFRESHCACHE_HEADER,
        &[refreshed.to_string(), results.len().to_string()],
    )];
    for (site, result) in &results {
        lines.push(match result {
            Ok(count) => command_format(
                command,
                REFRESHCACHE_OK,
                &[site.label().to_string(), count.to_string()],
            ),
            Err(e) => command_format(
                command,
                REFRESHCACHE_FAIL,
                &[site.label().to_string(), e.clone()],
            ),
        });
    }

    command
        .edit_response(ctx, EditInteractionResponse::new().content(lines.join("\n")))
        .await
        .ok();
}
