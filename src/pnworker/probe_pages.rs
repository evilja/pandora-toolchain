use crate::pnworker::messages::{PROBE_PAGE, format_message};
use serenity::all::{ButtonStyle, CreateActionRow, CreateButton};

// Discord caps an embed field value at 1024 characters, so a torrent with more files than that
// used to lose its tail to `truncate_embed_value`. The list is chunked into pages instead and the
// probe message carries prev/next buttons; the chunk budget leaves room for the page marker line.
pub const PROBE_PAGE_LINES: usize = 10;
pub const PROBE_PAGE_CHARS: usize = 900;

const PROBE_COMPONENT_PREFIX: &str = "pnprobe";

pub fn probe_pages(list: &str) -> Vec<String> {
    let mut pages: Vec<String> = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut chars = 0usize;
    for line in list.lines() {
        let cost = line.chars().count() + 1;
        let full = current.len() >= PROBE_PAGE_LINES || chars + cost > PROBE_PAGE_CHARS;
        if full && !current.is_empty() {
            pages.push(current.join("\n"));
            current.clear();
            chars = 0;
        }
        current.push(line);
        chars += cost;
    }
    if !current.is_empty() {
        pages.push(current.join("\n"));
    }
    pages
}

// The embed body for a 1-based page. A list that fits on one page renders exactly as before, with
// no page marker, so single-file torrents gain nothing to read past.
pub fn probe_page_body(list: &str, page: usize, lang: &str) -> String {
    let pages = probe_pages(list);
    if pages.len() <= 1 {
        return list.trim_end().to_string();
    }
    let index = page.clamp(1, pages.len()) - 1;
    format!(
        "{}\n\n{}",
        pages[index],
        format_message(
            PROBE_PAGE,
            lang,
            &[(index + 1).to_string(), pages.len().to_string()],
        ),
    )
}

pub fn probe_page_count(list: &str) -> usize {
    probe_pages(list).len()
}

pub fn probe_page_components(job_id: u64, page: usize, total_pages: usize) -> Vec<CreateActionRow> {
    if total_pages <= 1 {
        return Vec::new();
    }
    let page = page.clamp(1, total_pages);
    vec![CreateActionRow::Buttons(vec![
        CreateButton::new(probe_component_id(job_id, page.saturating_sub(1).max(1)))
            .label("◀")
            .style(ButtonStyle::Secondary)
            .disabled(page == 1),
        CreateButton::new(probe_component_id(job_id, (page + 1).min(total_pages)))
            .label("▶")
            .style(ButtonStyle::Secondary)
            .disabled(page == total_pages),
    ])]
}

pub fn probe_component_id(job_id: u64, page: usize) -> String {
    format!("{}:{}:{}", PROBE_COMPONENT_PREFIX, job_id, page)
}

pub fn parse_probe_component_id(id: &str) -> Option<(u64, usize)> {
    let mut parts = id.split(':');
    if parts.next()? != PROBE_COMPONENT_PREFIX {
        return None;
    }
    let job_id = parts.next()?.parse::<u64>().ok()?;
    let page = parts.next()?.parse::<usize>().ok()?;
    if parts.next().is_some() || page == 0 {
        return None;
    }
    Some((job_id, page))
}

// A page swap rebuilds the embed it was clicked on, so it has to find the file list among fields
// whose names are in whatever language the guild runs. Probe rows are the only embed content that
// starts a line with a backticked index followed by an em dash.
pub fn is_probe_list_value(value: &str) -> bool {
    value
        .lines()
        .any(|line| line.starts_with('`') && line[1..].contains("` — "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numbered_list(count: usize) -> String {
        (1..=count)
            .map(|index| format!("`{}` — E{}", index, index))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn short_lists_stay_on_a_single_unmarked_page() {
        let list = numbered_list(4);
        assert_eq!(probe_page_count(&list), 1);
        assert_eq!(probe_page_body(&list, 1, "en"), list);
        assert!(probe_page_components(7, 1, 1).is_empty());
    }

    #[test]
    fn long_lists_split_by_line_count() {
        let list = numbered_list(24);
        let pages = probe_pages(&list);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].lines().count(), PROBE_PAGE_LINES);
        assert_eq!(pages[2].lines().count(), 4);
        assert!(pages[1].starts_with("`11` — E11"));
    }

    #[test]
    fn long_lines_split_before_the_field_limit() {
        let list = (1..=6)
            .map(|index| format!("`{}` — {} (700MB)", index, "n".repeat(200)))
            .collect::<Vec<_>>()
            .join("\n");
        for page in probe_pages(&list) {
            assert!(page.chars().count() <= PROBE_PAGE_CHARS, "{}", page.len());
        }
    }

    #[test]
    fn paged_bodies_carry_a_clamped_page_marker() {
        let list = numbered_list(24);
        let body = probe_page_body(&list, 9, "en");
        assert!(body.starts_with("`21` — E21"));
        assert!(body.ends_with("3/3"));
    }

    #[test]
    fn component_ids_round_trip_and_reject_foreign_ids() {
        assert_eq!(
            parse_probe_component_id(&probe_component_id(1298, 4)),
            Some((1298, 4))
        );
        assert_eq!(parse_probe_component_id("pnhelp:sec:42"), None);
        assert_eq!(parse_probe_component_id("pnprobe:1298:0"), None);
        assert_eq!(parse_probe_component_id("pnprobe:1298:2:3"), None);
    }

    #[test]
    fn probe_rows_are_told_apart_from_other_embed_fields() {
        assert!(is_probe_list_value("`3` — E12\n`4` — E13"));
        assert!(is_probe_list_value("`0` — video.mkv (700MB)"));
        assert!(!is_probe_list_value("`1298935918741274624`"));
        assert!(!is_probe_list_value("⚙️ Encoding"));
        assert!(!is_probe_list_value("https://nyaa.si/view/123"));
    }
}
