use super::*;
use pandora_toolchain::pnworker::messages::*;
use serenity::all::{ButtonStyle, ComponentInteraction, CreateButton, CreateEmbed, CreateEmbedFooter};

const PAGES: [(&str, &str); 4] = [
    (TUTORIAL_1_INTRO_TITLE, TUTORIAL_1_INTRO_BODY),
    (TUTORIAL_1_ENCODE_TITLE, TUTORIAL_1_ENCODE_BODY),
    (TUTORIAL_1_PROBE_TITLE, TUTORIAL_1_PROBE_BODY),
    (TUTORIAL_1_TEAM_TITLE, TUTORIAL_1_TEAM_BODY),
];

fn tutorial_page(owner: u64, page: usize, lang: &str) -> CreateInteractionResponseMessage {
    let (title, body) = PAGES[page];
    CreateInteractionResponseMessage::new()
        .embed(CreateEmbed::new()
            .title(get_message(title, lang))
            .description(get_message(body, lang))
            .footer(CreateEmbedFooter::new(format!("{} / {}", page + 1, PAGES.len()))))
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("pntutorial:{owner}:{}", page.saturating_sub(1)))
                .label(get_message(TUTORIAL_PREVIOUS, lang))
                .style(ButtonStyle::Secondary)
                .disabled(page == 0),
            CreateButton::new(format!("pntutorial:{owner}:{}", (page + 1).min(PAGES.len() - 1)))
                .label(get_message(TUTORIAL_NEXT, lang))
                .style(ButtonStyle::Primary)
                .disabled(page + 1 == PAGES.len()),
        ])])
}

fn parse_tutorial_page(id: &str, user: u64) -> Option<usize> {
    let mut parts = id.split(':');
    if parts.next()? != "pntutorial" || parts.next()?.parse::<u64>().ok()? != user {
        return None;
    }
    let page = parts.next()?.parse::<usize>().ok()?;
    (parts.next().is_none() && page < PAGES.len()).then_some(page)
}

pub async fn handle_tutorial(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let response = tutorial_page(command.user.id.get(), 0, &read_lang(command.guild_id))
        .ephemeral(true);
    if let Err(error) = command.create_response(ctx, CreateInteractionResponse::Message(response)).await {
        report_interaction_failure("tutorial reply", command, &error);
    }
}

pub async fn handle_tutorial_component(ctx: &Context, component: &ComponentInteraction) {
    let response = match parse_tutorial_page(&component.data.custom_id, component.user.id.get()) {
        Some(page) => CreateInteractionResponse::UpdateMessage(tutorial_page(
            component.user.id.get(), page, &read_lang(component.guild_id),
        )),
        None => CreateInteractionResponse::Acknowledge,
    };
    if let Err(error) = component.create_response(ctx, response).await {
        eprintln!("tutorial page reply failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigation_accepts_only_valid_pages_for_the_owner() {
        for page in 0..PAGES.len() {
            assert_eq!(parse_tutorial_page(&format!("pntutorial:42:{page}"), 42), Some(page));
        }
        for id in ["pntutorial:42:4", "pntutorial:42:-1", "pntutorial:42:0:extra",
                   "pntutorial:43:0", "pntutorial:42", "pnhelp:42:0"] {
            assert_eq!(parse_tutorial_page(id, 42), None);
        }
    }
}
