use super::*;

pub async fn handle_merge(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let mut response_msg = match working_response(ctx, command, "Working…").await {
        Some(m) => m,
        None => return,
    };
    let result = match smartcode_merge_upload(ctx, command, &mut response_msg, "/merge", "merge").await {
        Some(r) => r,
        None => return,
    };
    let embed = CreateEmbed::new()
        .title("Merge complete")
        .field("Repo", format!("`{}`", result.owner_repo), true)
        .field("Release", format!("`{}`", result.release_path), true)
        .field("Source", format!("`{}`", result.source_path), true)
        .field("Warnings", format_warnings_field(&result.warnings), false);
    let _ = response_msg.edit(ctx, EditMessage::new().content("").embed(embed)).await;
}
