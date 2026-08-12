use super::*;
use pandora_toolchain::lib::http::acix::fetch_fansub_templates;
use pandora_toolchain::lib::http::anizm::fetch_publishing_catalog;
use pandora_toolchain::lib::http::openanime::fetch_fansubs;
use pandora_toolchain::pnworker::server_config::FansubSite;
use serenity::builder::CreateAutocompleteResponse;

const MAX_FANSUB_CHOICES: usize = 25;
const MAX_CHOICE_LABEL_CHARS: usize = 100;

// One selectable fansub for one site. `value` is exactly what `/edit` stores in `meta.pandora`, so
// it is the site's own identifier — an AnimeciX template id, an OpenAnime `fansubSecureName`, or an
// Anizm staff-form fansub id — never the human-readable name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FansubOption {
    pub value: String,
    pub name: String,
    pub suffix: Option<String>,
    terms: Vec<String>,
}

impl FansubOption {
    pub fn new(value: String, name: String, suffix: Option<String>, extra_terms: &[&str]) -> Self {
        let mut terms = vec![value.to_lowercase(), name.to_lowercase()];
        terms.extend(
            extra_terms
                .iter()
                .filter(|term| !term.trim().is_empty())
                .map(|term| term.to_lowercase()),
        );
        Self {
            value,
            name,
            suffix,
            terms,
        }
    }

    pub fn choice_label(&self) -> String {
        let suffix = self
            .suffix
            .as_ref()
            .map(|suffix| format!(" ({})", suffix))
            .unwrap_or_default();
        let available = MAX_CHOICE_LABEL_CHARS.saturating_sub(suffix.chars().count());
        let name = self.name.chars().take(available).collect::<String>();
        format!("{}{}", name, suffix)
    }

    pub fn display(&self) -> String {
        match &self.suffix {
            Some(suffix) => format!("{} ({})", self.name, suffix),
            None => self.name.clone(),
        }
    }
}

pub async fn site_fansubs(site: FansubSite) -> Result<Vec<FansubOption>, String> {
    match site {
        FansubSite::AnimeciX => Ok(fetch_fansub_templates()
            .await?
            .into_iter()
            .map(|template| {
                FansubOption::new(
                    template.id.to_string(),
                    template.display_name(),
                    Some(format!("#{}", template.id)),
                    &[&template.name, &template.translator],
                )
            })
            .collect()),
        FansubSite::OpenAnime => Ok(fetch_fansubs()
            .await?
            .into_iter()
            .map(|fansub| {
                FansubOption::new(
                    fansub.secure_name.clone(),
                    fansub.display_name(),
                    None,
                    &[&fansub.name, &fansub.secure_name],
                )
            })
            .collect()),
        FansubSite::Anizm => Ok(fetch_publishing_catalog()
            .await?
            .fansubs
            .into_iter()
            .map(|fansub| {
                FansubOption::new(
                    fansub.id.to_string(),
                    fansub.label.clone(),
                    Some(format!("#{}", fansub.id)),
                    &[&fansub.label],
                )
            })
            .collect()),
    }
}

// The stored selection is re-checked against the live directory by identifier, so a renamed fansub
// keeps working and a hand-typed name that does not correspond to a real id is refused.
pub async fn resolve_fansub_selection(
    site: FansubSite,
    value: &str,
) -> Result<FansubOption, String> {
    let value = value.trim();
    let options = site_fansubs(site)
        .await
        .map_err(|e| format!("failed to load {} fansubs: {}", site.label(), e))?;
    options
        .iter()
        .find(|option| option.value == value)
        .or_else(|| {
            options
                .iter()
                .find(|option| option.value.eq_ignore_ascii_case(value))
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "`{}` is not a known {} fansub. Pick one from the search results.",
                value,
                site.label()
            )
        })
}

pub fn filter_fansub_options(options: &[FansubOption], partial: &str) -> Vec<FansubOption> {
    let partial = partial.trim().to_lowercase();
    let mut matches = options
        .iter()
        .filter(|option| {
            partial.is_empty() || option.terms.iter().any(|term| term.contains(&partial))
        })
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        match_priority(left, &partial)
            .cmp(&match_priority(right, &partial))
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.value.cmp(&right.value))
    });
    matches.truncate(MAX_FANSUB_CHOICES);
    matches
}

fn match_priority(option: &FansubOption, partial: &str) -> u8 {
    if option.terms.iter().any(|term| term == partial) {
        0
    } else if option
        .terms
        .iter()
        .any(|term| term.starts_with(partial))
    {
        1
    } else {
        2
    }
}

// A directory lookup that fails and one that simply has no match both render as Discord's "no
// options" box, which hides an expired login or an unreachable staff panel behind what looks like a
// bad search term. The reason is surfaced as a single choice instead; picking it stores nothing,
// because `resolve_fansub_selection` rejects the sentinel like any other non-identifier.
const LOOKUP_FAILED_VALUE: &str = "__lookup_failed__";

pub async fn fansub_autocomplete(
    ctx: &Context,
    interaction: &serenity::all::CommandInteraction,
    site: FansubSite,
    partial: &str,
) {
    let mut response = CreateAutocompleteResponse::new();
    match site_fansubs(site).await {
        Ok(options) => {
            for option in filter_fansub_options(&options, partial) {
                response = response.add_string_choice(option.choice_label(), option.value.clone());
            }
        }
        Err(e) => {
            eprintln!("[{}] fansub autocomplete failed: {}", site.option_name(), e);
            response =
                response.add_string_choice(lookup_failed_label(site, &e), LOOKUP_FAILED_VALUE);
        }
    }
    interaction
        .create_response(ctx, CreateInteractionResponse::Autocomplete(response))
        .await
        .ok();
}

fn lookup_failed_label(site: FansubSite, error: &str) -> String {
    let prefix = format!("⚠ {} lookup failed: ", site.label());
    let available = MAX_CHOICE_LABEL_CHARS.saturating_sub(prefix.chars().count());
    let reason = error.split_whitespace().collect::<Vec<_>>().join(" ");
    if reason.chars().count() <= available {
        return format!("{}{}", prefix, reason);
    }
    let truncated = reason
        .chars()
        .take(available.saturating_sub(1))
        .collect::<String>();
    format!("{}{}…", prefix, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(value: &str, name: &str, suffix: Option<&str>, terms: &[&str]) -> FansubOption {
        FansubOption::new(
            value.to_string(),
            name.to_string(),
            suffix.map(str::to_string),
            terms,
        )
    }

    #[test]
    fn filters_by_identifier_name_and_extra_terms() {
        let options = vec![
            option("50", "Akira Fansub — AkiraSubs", Some("#50"), &["Akira Fansub", "AkiraSubs"]),
            option("218", "SomeSub", Some("#218"), &["SomeSub"]),
            option("akira-subs", "Akira Subs — akira-subs", None, &["Akira Subs", "akira-subs"]),
        ];
        assert_eq!(filter_fansub_options(&options, "somesub")[0].value, "218");
        assert_eq!(filter_fansub_options(&options, "218")[0].value, "218");
        assert_eq!(filter_fansub_options(&options, "akira-subs")[0].value, "akira-subs");
        assert_eq!(filter_fansub_options(&options, "akira").len(), 2);
    }

    #[test]
    fn exact_identifier_matches_sort_first() {
        let options = vec![
            option("1", "Akira Fansub Extended", Some("#1"), &[]),
            option("2", "Akira", Some("#2"), &[]),
        ];
        assert_eq!(filter_fansub_options(&options, "akira")[0].value, "2");
    }

    #[test]
    fn choice_labels_fit_the_discord_limit_and_keep_the_identifier() {
        let long = option("50", &"x".repeat(150), Some("#50"), &[]);
        let label = long.choice_label();
        assert!(label.chars().count() <= MAX_CHOICE_LABEL_CHARS);
        assert!(label.ends_with("(#50)"));

        let secure = option("akira-subs", "Akira Subs — akira-subs", None, &[]);
        assert_eq!(secure.choice_label(), "Akira Subs — akira-subs");
    }

    #[test]
    fn lookup_failures_are_reported_inside_the_label_limit() {
        let short = lookup_failed_label(FansubSite::Anizm, "Anizm email is empty.");
        assert_eq!(short, "⚠ Anizm lookup failed: Anizm email is empty.");

        let long = lookup_failed_label(FansubSite::Anizm, &"detail ".repeat(60));
        assert!(long.chars().count() <= MAX_CHOICE_LABEL_CHARS, "{}", long);
        assert!(long.ends_with('…'), "{}", long);
    }

    #[test]
    fn choices_are_capped_at_the_discord_limit() {
        let options = (0..40)
            .map(|index| option(&index.to_string(), &format!("Group {:02}", index), None, &[]))
            .collect::<Vec<_>>();
        assert_eq!(filter_fansub_options(&options, "group").len(), MAX_FANSUB_CHOICES);
    }
}
