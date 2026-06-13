use super::*;

pub async fn handle_attach(ctx: &Context, command: &serenity::all::CommandInteraction) {
    run_attach_or_init(ctx, command, false).await;
}
