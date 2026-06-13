use super::*;

pub async fn handle_init(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, true).await;
}
