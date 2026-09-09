use super::*;
use pandora_toolchain::pnworker::messages::*;
use serenity::all::{ButtonStyle, ComponentInteraction, CreateButton, CreateEmbed, CreateEmbedFooter};

const PAGES: [(&str, &str); 5] = [
    (TUTORIAL_1_INTRO_TITLE, TUTORIAL_1_INTRO_BODY),
    (TUTORIAL_1_ENCODE_TITLE, TUTORIAL_1_ENCODE_BODY),
    (TUTORIAL_1_PROBE_TITLE, TUTORIAL_1_PROBE_BODY),
    (TUTORIAL_1_TEAM_TITLE, TUTORIAL_1_TEAM_BODY),
    (TUTORIAL_1_SAVED_TITLE, TUTORIAL_1_SAVED_BODY),
];

const ADMIN_PAGES: [(&str, &str); 7] = [
    (TUTORIAL_ADMIN_START_TITLE, TUTORIAL_ADMIN_START_BODY),
    (TUTORIAL_ADMIN_INIT_TITLE, TUTORIAL_ADMIN_INIT_BODY),
    (TUTORIAL_ADMIN_EDIT_TITLE, TUTORIAL_ADMIN_EDIT_BODY),
    (TUTORIAL_ADMIN_DELIVERY_TITLE, TUTORIAL_ADMIN_DELIVERY_BODY),
    (TUTORIAL_ADMIN_MEDIA_TITLE, TUTORIAL_ADMIN_MEDIA_BODY),
    (TUTORIAL_ADMIN_AUTH_TITLE, TUTORIAL_ADMIN_AUTH_BODY),
    (TUTORIAL_ADMIN_EXTRAS_TITLE, TUTORIAL_ADMIN_EXTRAS_BODY),
];

fn pages(lesson: &str) -> Option<&'static [(&'static str, &'static str)]> {
    match lesson { "1" => Some(&PAGES), "admin" => Some(&ADMIN_PAGES), _ => None }
}

fn tutorial_page(owner: u64, lesson: &str, page: usize, lang: &str) -> CreateInteractionResponseMessage {
    let pages = pages(lesson).expect("validated tutorial lesson");
    let (title, body) = pages[page];
    CreateInteractionResponseMessage::new()
        .embed(CreateEmbed::new()
            .title(get_message(title, lang))
            .description(get_message(body, lang))
            .footer(CreateEmbedFooter::new(format!("{} / {}", page + 1, pages.len()))))
        .components(vec![CreateActionRow::Buttons(vec![
            CreateButton::new(format!("pntutorial:{owner}:{lesson}:{}", page.saturating_sub(1)))
                .label(get_message(TUTORIAL_PREVIOUS, lang))
                .style(ButtonStyle::Secondary)
                .disabled(page == 0),
            CreateButton::new(format!("pntutorial:{owner}:{lesson}:{}", (page + 1).min(pages.len() - 1)))
                .label(get_message(TUTORIAL_NEXT, lang))
                .style(ButtonStyle::Primary)
                .disabled(page + 1 == pages.len()),
        ])])
}

fn parse_tutorial_page(id: &str, user: u64) -> Option<(&str, usize)> {
    let mut parts = id.split(':');
    if parts.next()? != "pntutorial" || parts.next()?.parse::<u64>().ok()? != user { return None; }
    let lesson_or_page = parts.next()?;
    // Older beginner messages remain navigable after the admin lesson is added.
    let (lesson, page) = match parts.next() {
        Some(page) => (lesson_or_page, page.parse::<usize>().ok()?),
        None => ("1", lesson_or_page.parse::<usize>().ok()?),
    };
    (parts.next().is_none() && page < pages(lesson)?.len()).then_some((lesson, page))
}

pub async fn handle_tutorial(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let lesson = command.data.options.first().map(|o| o.name.as_str()).unwrap_or("1");
    if pages(lesson).is_none() { return; }
    let response = tutorial_page(command.user.id.get(), lesson, 0, &read_lang(command.guild_id))
        .ephemeral(true);
    if let Err(error) = command.create_response(ctx, CreateInteractionResponse::Message(response)).await {
        report_interaction_failure("tutorial reply", command, &error);
    }
}

pub async fn handle_tutorial_component(ctx: &Context, component: &ComponentInteraction) {
    let response = match parse_tutorial_page(&component.data.custom_id, component.user.id.get()) {
        Some((lesson, page)) => CreateInteractionResponse::UpdateMessage(tutorial_page(
            component.user.id.get(), lesson, page, &read_lang(component.guild_id),
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
        for lesson in ["1", "admin"] {
            for page in 0..pages(lesson).unwrap().len() {
                let id = format!("pntutorial:42:{lesson}:{page}");
                assert_eq!(parse_tutorial_page(&id, 42), Some((lesson, page)));
            }
        }
        assert_eq!(parse_tutorial_page("pntutorial:42:0", 42), Some(("1", 0)));
        for id in ["pntutorial:42:1:5", "pntutorial:42:admin:7", "pntutorial:42:-1", "pntutorial:42:1:0:extra",
                   "pntutorial:43:1:0", "pntutorial:42", "pnhelp:42:0", "pntutorial:42:unknown:0"] {
            assert_eq!(parse_tutorial_page(id, 42), None);
        }
    }

    #[test]
    fn every_lesson_page_fits_discord_limits_in_all_languages() {
        for lang in ["en", "tr", "jp"] {
            for (title, body) in PAGES.iter().chain(ADMIN_PAGES.iter()) {
                assert!(get_message(title, lang).encode_utf16().count() <= 256);
                assert!(get_message(body, lang).encode_utf16().count() <= 4096);
            }
        }
    }
}
