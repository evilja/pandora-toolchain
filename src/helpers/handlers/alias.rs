use super::*;

use pandora_toolchain::lib::alias::{read_aliases, set_alias, write_aliases};

// The rank `/alias force` needs: renaming somebody else is an admin's job, while renaming yourself
// is not. Both live on one command, so the tier is checked here rather than in the ranks table.
const FORCE_RANK: u8 = 2;

pub async fn handle_alias(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let subcommand = subcommand_options(command)
        .map(|(name, _)| name)
        .unwrap_or("choose");
    let (target, target_label) = match subcommand {
        "choose" => (command.user.id.get(), "You are".to_string()),
        "force" => {
            if !has_level_at_least(command.user.id.get(), FORCE_RANK) {
                command_error(ctx, command, "Sorry, you're not a sigma.").await;
                return;
            }
            let Some(user) = option_user(command, "user") else {
                command_error(ctx, command, "Error: `user` is required.").await;
                return;
            };
            (user.get(), format!("<@{}> is", user.get()))
        }
        other => {
            command_error(ctx, command, format!("Unknown alias subcommand `{}`.", other)).await;
            return;
        }
    };
    let Some(name) = option_trimmed(command, "name") else {
        command_error(ctx, command, "Error: `name` is required. Use `-` to clear an alias.").await;
        return;
    };

    let mut aliases = read_aliases().await;
    let changed = set_alias(&mut aliases, target, &name);
    if changed {
        if let Err(e) = write_aliases(&aliases).await {
            command_error(ctx, command, format!("Failed to write aliases: {}", e)).await;
            return;
        }
    }

    let cleared = matches!(name.as_str(), "-");
    let description = if cleared {
        format!("{} credited under their Discord name again.", target_label)
    } else {
        format!("{} credited as `{}` from now on.", target_label, name)
    };
    command
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .embed(success_embed(command, COMMAND_UPDATED).description(description))
                    .ephemeral(true),
            ),
        )
        .await
        .ok();
}

// What `%enc%` becomes for one person: the alias they chose, else the name Discord shows for them.
// A server nickname is deliberately not consulted — an alias is global, and a credit that changed
// with the guild the command was typed in would not be the same person's name any more.
pub async fn credited_name(user: &serenity::all::User) -> String {
    if let Some(alias) = pandora_toolchain::lib::alias::alias_for(user.id.get()).await {
        return alias;
    }
    user.global_name.clone().unwrap_or_else(|| user.name.clone())
}
