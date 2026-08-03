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
    let embed = success_embed(command, COMMAND_MERGE_COMPLETE)
        .field(
            command_message(command, FIELD_REPO),
            format!("`{}`", result.owner_repo),
            true,
        )
        .field(
            command_message(command, FIELD_RELEASE),
            format!("`{}`", result.release_path),
            true,
        )
        .field(
            command_message(command, FIELD_SOURCE),
            format!("`{}`", result.source_path),
            false,
        )
        .field(
            command_message(command, FIELD_WARNINGS),
            format_warnings_field(&result.warnings, command),
            false,
        );
    edit_response_embed(ctx, &mut response_msg, embed).await;
}
