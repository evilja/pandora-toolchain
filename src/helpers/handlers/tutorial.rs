use super::*;
use pandora_toolchain::pnworker::messages::*;

pub async fn handle_tutorial(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let lang = read_lang(command.guild_id);
    let mut response = CreateInteractionResponseMessage::new().ephemeral(true);
    for (title, body) in [
        (TUTORIAL_1_INTRO_TITLE, TUTORIAL_1_INTRO_BODY),
        (TUTORIAL_1_ENCODE_TITLE, TUTORIAL_1_ENCODE_BODY),
        (TUTORIAL_1_PROBE_TITLE, TUTORIAL_1_PROBE_BODY),
        (TUTORIAL_1_TEAM_TITLE, TUTORIAL_1_TEAM_BODY),
    ] {
        response = response.add_embed(
            serenity::all::CreateEmbed::new()
                .title(get_message(title, &lang))
                .description(get_message(body, &lang)),
        );
    }
    if let Err(error) = command.create_response(ctx, CreateInteractionResponse::Message(response)).await {
        report_interaction_failure("tutorial reply", command, &error);
    }
}
