use super::*;
use pandora_toolchain::lib::http::acix::{fetch_fansub_templates, FansubTemplate};
use serenity::builder::CreateAutocompleteResponse;

const MAX_FANSUB_CHOICES: usize = 25;
const MAX_CHOICE_LABEL_CHARS: usize = 100;

fn fansub_choice_label(template: &FansubTemplate) -> String {
    let suffix = format!(" (#{})", template.id);
    let available = MAX_CHOICE_LABEL_CHARS.saturating_sub(suffix.chars().count());
    let name = template.display_name().chars().take(available).collect::<String>();
    format!("{}{}", name, suffix)
}

fn filter_fansub_templates(templates: &[FansubTemplate], partial: &str) -> Vec<FansubTemplate> {
    let partial = partial.trim().to_lowercase();
    let mut matches = templates.iter()
        .filter(|template| {
            partial.is_empty()
                || template.id.to_string().contains(&partial)
                || template.name.to_lowercase().contains(&partial)
                || template.translator.to_lowercase().contains(&partial)
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        let left_name = left.display_name().to_lowercase();
        let right_name = right.display_name().to_lowercase();
        let left_priority = if left.id.to_string() == partial || left_name == partial {
            0
        } else if left_name.starts_with(&partial) || left.translator.to_lowercase().starts_with(&partial) {
            1
        } else {
            2
        };
        let right_priority = if right.id.to_string() == partial || right_name == partial {
            0
        } else if right_name.starts_with(&partial) || right.translator.to_lowercase().starts_with(&partial) {
            1
        } else {
            2
        };
        left_priority.cmp(&right_priority)
            .then_with(|| left_name.cmp(&right_name))
            .then_with(|| left.id.cmp(&right.id))
    });
    matches.truncate(MAX_FANSUB_CHOICES);
    matches
}

pub async fn handle_acixtemplate_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
) {
    let partial = interaction.data.autocomplete()
        .filter(|option| option.name == "template")
        .map(|option| option.value.to_string())
        .unwrap_or_default();
    let mut response = CreateAutocompleteResponse::new();
    match fetch_fansub_templates().await {
        Ok(templates) => {
            for template in filter_fansub_templates(&templates, &partial) {
                response = response.add_string_choice(
                    fansub_choice_label(&template),
                    template.id.to_string(),
                );
            }
        }
        Err(e) => eprintln!("[acixtemplate] autocomplete failed: {}", e),
    }
    interaction.create_response(ctx, CreateInteractionResponse::Autocomplete(response)).await.ok();
}

pub async fn handle_acixtemplate(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
) {
    let server_id = match command_server_id(ctx, command, "/acixtemplate").await {
        Some(id) => id,
        None => return,
    };
    let template_id = match option_str(command, "template")
        .and_then(|value| value.trim().parse::<i64>().ok())
        .filter(|value| *value > 0)
    {
        Some(value) => value,
        None => {
            command_error(ctx, command, "Error: select a fansub from the AnimeciX search results.").await;
            return;
        }
    };

    let template = match fetch_fansub_templates().await {
        Ok(templates) => match templates.into_iter().find(|template| template.id == template_id) {
            Some(template) => template,
            None => {
                command_error(ctx, command, format!("Error: AnimeciX fansub template `{}` does not exist.", template_id)).await;
                return;
            }
        },
        Err(e) => {
            command_error(ctx, command, format!("Failed to load AnimeciX fansubs: {}", e)).await;
            return;
        }
    };

    if let Err(e) = write_server_acix_template(server_id, template.id).await {
        command_error(ctx, command, format!("Failed to save server template: {}", e)).await;
        return;
    }

    command.create_response(ctx, CreateInteractionResponse::Message(
        CreateInteractionResponseMessage::new()
            .content(format!(
                "AnimeciX fansub template for server `{}` set to **{}** (`{}`).",
                server_id,
                template.display_name(),
                template.id,
            ))
            .ephemeral(true)
    )).await.ok();
}

#[cfg(test)]
mod tests {
    use super::{fansub_choice_label, filter_fansub_templates, FansubTemplate, MAX_CHOICE_LABEL_CHARS};
    use super::super::{server_meta_with_acix_template, SERVER_ACIX_TEMPLATE_LINE};

    fn template(id: i64, name: &str, translator: &str) -> FansubTemplate {
        FansubTemplate {
            id,
            name: name.to_string(),
            translator: translator.to_string(),
        }
    }

    #[test]
    fn filters_fansubs_by_name_translator_and_id() {
        let templates = vec![
            template(50, "Akira Fansub", "AkiraSubs"),
            template(218, "SomeSub", "SomeSub"),
        ];
        assert_eq!(filter_fansub_templates(&templates, "akira")[0].id, 50);
        assert_eq!(filter_fansub_templates(&templates, "somesub")[0].id, 218);
        assert_eq!(filter_fansub_templates(&templates, "218")[0].id, 218);
    }

    #[test]
    fn choice_labels_fit_discord_limit() {
        let label = fansub_choice_label(&template(50, &"x".repeat(150), "AkiraSubs"));
        assert!(label.chars().count() <= MAX_CHOICE_LABEL_CHARS);
        assert!(label.ends_with("(#50)"));
    }

    #[test]
    fn server_template_preserves_existing_meta_lines() {
        let meta = (0..=12).map(|line| format!("line{}", line)).collect::<Vec<_>>().join("\n");
        let updated = server_meta_with_acix_template(&meta, 218);
        let lines = updated.lines().collect::<Vec<_>>();
        assert_eq!(lines[0], "line0");
        assert_eq!(lines[12], "line12");
        assert_eq!(lines[SERVER_ACIX_TEMPLATE_LINE], "218");
    }
}
