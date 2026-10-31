use super::*;
use pandora_toolchain::lib::mpeg::signature::{self, MAX_LABEL_CHARS};
use pandora_toolchain::pnworker::messages::{
    SIGNATURE_CURRENT, SIGNATURE_UPDATED, SIGNATURE_INVALID, SIGNATURE_FAILED, SIGNATURE_DENIED,
};

pub async fn handle_signature(ctx: &Context, command: &serenity::all::CommandInteraction) {
    // This deployment-wide setting remains Witch-only even if a command-rank file is edited.
    if !has_level_at_least(command.user.id.get(), 4) {
        command_error(ctx, command, command_message(command, SIGNATURE_DENIED)).await;
        return;
    }
    let updated = if let Some(text) = option_str(command, "text") {
        if !signature::valid_label(text) {
            command_error(ctx, command, command_format(command, SIGNATURE_INVALID, &[MAX_LABEL_CHARS.to_string()])).await;
            return;
        }
        if let Err(error) = signature::write_label(text).await {
            command_error(ctx, command, command_format(command, SIGNATURE_FAILED, &[error])).await;
            return;
        }
        true
    } else {
        false
    };
    let description = command_format(
        command,
        if updated { SIGNATURE_UPDATED } else { SIGNATURE_CURRENT },
        &[signature::video_identifier()],
    );
    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .embed(success_embed(command, COMMAND_UPDATED).description(description))
            .ephemeral(true)
    )).await.ok();
}
