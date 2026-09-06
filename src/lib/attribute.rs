use std::path::PathBuf;

// Every dialogue Pandora injects is stamped with this in the event's Name (actor) field. It is what
// makes the injection repeatable: a merge strips every line carrying it before adding the current
// ones, so re-merging an episode replaces its credits instead of stacking a second copy on top of
// the first. It also keeps the lines identifiable in a repo release somebody opens by hand.
pub const PANDORA_ACTOR: &str = "PandoraIdentifier";

// The event fields, in order: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect,
// Text. Only the last may contain commas, so a line is split into exactly this many parts.
const EVENT_FIELDS: usize = 10;
const STYLE_FIELD: usize = 3;
const ACTOR_FIELD: usize = 4;

// Script Info keys copied from the styles file along with the styles themselves. A style is written
// against a canvas: the same border, shadow and font size mean different things at 1280x720 than at
// 1920x1080, so taking the styles without these would render them at a size nobody chose. WrapStyle
// is deliberately not among them — that one is the server's `/edit wrapstyle` setting.
const CANVAS_KEYS: [&str; 3] = ["PlayResX", "PlayResY", "ScaledBorderAndShadow"];

pub struct StyledAss {
    pub text: String,
    // Styles the events still ask for that the new style list does not define. libass silently
    // renders those lines in Default, so they are reported rather than corrected.
    pub missing_styles: Vec<String>,
}

// A dialogue as it will be stored and injected: the caller's line, validated, with the actor field
// stamped. Accepts a line with or without its `Dialogue:` prefix, since both are what people copy.
pub fn normalize_dialogue(line: &str) -> Result<String, String> {
    let body = line.trim();
    let body = strip_event_prefix(body, "dialogue:").unwrap_or(body);
    if body.is_empty() {
        return Err("a dialogue line cannot be empty".to_string());
    }
    let mut fields = split_event_fields(body);
    if fields.len() < EVENT_FIELDS {
        return Err(format!(
            "a dialogue line needs {} comma-separated fields (Layer,Start,End,Style,Name,MarginL,MarginR,MarginV,Effect,Text), this one has {}",
            EVENT_FIELDS,
            fields.len()
        ));
    }
    for (index, label) in [(1usize, "start"), (2, "end")] {
        if !fields[index].contains(':') {
            return Err(format!(
                "the {} time `{}` is not an ASS timestamp (0:00:05.00)",
                label,
                fields[index].trim()
            ));
        }
    }
    if fields[EVENT_FIELDS - 1].trim().is_empty() {
        return Err("a dialogue line with no text would render nothing".to_string());
    }
    fields[ACTOR_FIELD] = PANDORA_ACTOR.to_string();
    Ok(format!("Dialogue: {}", fields.join(",")))
}

// `%key%` for every pair given. A key that is not in the list is left standing rather than blanked:
// `%enc%` survives a `/merge` with no encoder to name, so the `/smartcode` that encodes the episode
// later is what finally answers it.
pub fn substitute(text: &str, pairs: &[(&str, String)]) -> String {
    let mut out = text.to_string();
    for (key, value) in pairs {
        out = out.replace(&format!("%{}%", key), value);
    }
    out
}

// The styles an attribute file defines, in file order. Empty means it can supply none, which is
// the one thing a file uploaded to `/attribute set` has to be able to do.
pub fn style_names(source: &str) -> Vec<String> {
    let lines = read_lines(source);
    let Some(section) = find_section(&lines, is_styles_header) else {
        return Vec::new();
    };
    let block: Vec<String> = lines[section].iter().map(|line| line.to_string()).collect();
    defined_styles(&block)
}

// Replaces the merged script's style list with the one from the attribute file, and takes the
// canvas the styles were written against with it.
pub fn replace_styles(merged: &str, styles_source: &str) -> Result<StyledAss, String> {
    let newline = if merged.contains("\r\n") { "\r\n" } else { "\n" };
    let source_lines = read_lines(styles_source);
    let Some(source_styles) = find_section(&source_lines, is_styles_header) else {
        return Err("the attribute file has no [V4+ Styles] section".to_string());
    };
    let style_block: Vec<String> = source_lines[source_styles.clone()]
        .iter()
        .map(|line| line.to_string())
        .collect();

    let mut lines: Vec<String> = read_lines(merged).iter().map(|l| l.to_string()).collect();
    match find_section(&lines.iter().map(String::as_str).collect::<Vec<_>>(), is_styles_header) {
        Some(existing) => {
            lines.splice(existing, style_block.iter().cloned());
        }
        None => {
            let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
            let at = find_section(&refs, is_events_header)
                .map(|events| events.start)
                .unwrap_or(lines.len());
            lines.splice(at..at, style_block.iter().cloned());
        }
    }
    apply_canvas_keys(&mut lines, &source_lines);

    let defined = defined_styles(&style_block);
    let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
    let missing_styles = missing_event_styles(&refs, &defined);
    let mut text = lines.join(newline);
    text.push_str(newline);
    Ok(StyledAss {
        text,
        missing_styles,
    })
}

// Drops every previously injected line and appends the current ones to the events section. The
// order given is the order written, so a credit block reads the way it was entered.
pub fn inject_dialogues(ass: &str, dialogues: &[String]) -> String {
    let newline = if ass.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = read_lines(ass)
        .into_iter()
        .filter(|line| !is_injected_event(line))
        .map(|line| line.to_string())
        .collect();
    if dialogues.is_empty() {
        let mut text = lines.join(newline);
        text.push_str(newline);
        return text;
    }
    let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
    let at = match find_section(&refs, is_events_header) {
        Some(events) => {
            // Past the last line that is actually part of the section: a script can end with blank
            // lines, and an event written after them is still read, but it looks like a mistake.
            let mut end = events.end;
            while end > events.start && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            end
        }
        None => {
            lines.push("[Events]".to_string());
            lines.push(
                "Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text"
                    .to_string(),
            );
            lines.len()
        }
    };
    lines.splice(at..at, dialogues.iter().cloned());
    let mut text = lines.join(newline);
    text.push_str(newline);
    text
}

pub fn is_injected_event(line: &str) -> bool {
    let trimmed = line.trim_start();
    let Some(body) = strip_event_prefix(trimmed, "dialogue:")
        .or_else(|| strip_event_prefix(trimmed, "comment:"))
    else {
        return false;
    };
    split_event_fields(body)
        .get(ACTOR_FIELD)
        .map(|actor| actor.trim() == PANDORA_ACTOR)
        .unwrap_or(false)
}

fn strip_event_prefix<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    let head = line.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| line[prefix.len()..].trim_start())
}

fn split_event_fields(body: &str) -> Vec<String> {
    body.splitn(EVENT_FIELDS, ',')
        .map(str::to_string)
        .collect()
}

fn read_lines(text: &str) -> Vec<&str> {
    text.lines().map(|line| line.trim_end_matches('\r')).collect()
}

// A section header's inner text, lowercased: `[V4+ Styles]` reads as `v4+ styles`.
fn header_name(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('[')?.strip_suffix(']')?;
    Some(inner.trim().to_ascii_lowercase())
}

// `[V4 Styles]`, `[V4+ Styles]` and `[V4++ Styles]` are the same section under three script
// versions, and a file only ever carries one of them.
fn is_styles_header(name: &str) -> bool {
    name.ends_with("styles")
}

fn is_events_header(name: &str) -> bool {
    name == "events"
}

fn is_script_info_header(name: &str) -> bool {
    name == "script info"
}

// The half-open line range of the first section whose header matches, header line included.
fn find_section(
    lines: &[&str],
    matches: fn(&str) -> bool,
) -> Option<std::ops::Range<usize>> {
    let start = lines
        .iter()
        .position(|line| header_name(line).is_some_and(|name| matches(&name)))?;
    let end = lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| header_name(line).is_some())
        .map(|(index, _)| index)
        .unwrap_or(lines.len());
    Some(start..end)
}

fn apply_canvas_keys(lines: &mut Vec<String>, source_lines: &[&str]) {
    let Some(source_info) = find_section(source_lines, is_script_info_header) else {
        return;
    };
    for key in CANVAS_KEYS {
        let Some(value) = source_lines[source_info.clone()]
            .iter()
            .find_map(|line| script_info_value(line, key))
        else {
            continue;
        };
        let entry = format!("{}: {}", key, value);
        let refs = lines.iter().map(String::as_str).collect::<Vec<_>>();
        let Some(info) = find_section(&refs, is_script_info_header) else {
            // No Script Info at all: one is opened above whatever comes first, since the keys are
            // meaningless anywhere else.
            lines.splice(0..0, ["[Script Info]".to_string(), entry]);
            continue;
        };
        match lines[info.clone()]
            .iter()
            .position(|line| script_info_value(line, key).is_some())
        {
            Some(offset) => lines[info.start + offset] = entry,
            None => lines.insert(info.end, entry),
        }
    }
}

fn script_info_value(line: &str, key: &str) -> Option<String> {
    let (name, value) = line.split_once(':')?;
    name.trim()
        .eq_ignore_ascii_case(key)
        .then(|| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn defined_styles(style_block: &[String]) -> Vec<String> {
    style_block
        .iter()
        .filter_map(|line| {
            let body = strip_event_prefix(line.trim_start(), "style:")?;
            body.split(',').next().map(|name| name.trim().to_string())
        })
        .filter(|name| !name.is_empty())
        .collect()
}

fn missing_event_styles(lines: &[&str], defined: &[String]) -> Vec<String> {
    let Some(events) = find_section(lines, is_events_header) else {
        return Vec::new();
    };
    let mut missing: Vec<String> = Vec::new();
    for line in &lines[events] {
        let trimmed = line.trim_start();
        let Some(body) = strip_event_prefix(trimmed, "dialogue:")
            .or_else(|| strip_event_prefix(trimmed, "comment:"))
        else {
            continue;
        };
        let fields = split_event_fields(body);
        let Some(style) = fields.get(STYLE_FIELD).map(|style| style.trim().to_string()) else {
            continue;
        };
        // `*Default` is the "unknown style" spelling libass already writes; it names no style and
        // reporting it would be reporting the fallback as the problem.
        let bare = style.trim_start_matches('*');
        if bare.is_empty()
            || defined.iter().any(|known| known == bare)
            || missing.iter().any(|known| known == bare)
        {
            continue;
        }
        missing.push(bare.to_string());
    }
    missing
}

// ---------------------------------------------------------------------------------------------
// Stored attributes. A channel's styles file and dialogue list live beside its `meta.toml`, since
// they belong to the anime that channel is attached to and nothing else.

pub fn channel_dir(server_id: u64, channel_id: u64) -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join(channel_id.to_string())
}

pub fn styles_path(server_id: u64, channel_id: u64) -> PathBuf {
    channel_dir(server_id, channel_id).join("attribute.ass")
}

pub fn dialogues_path(server_id: u64, channel_id: u64) -> PathBuf {
    channel_dir(server_id, channel_id).join("attribute_dialogues.json")
}

// Every dialogue this server has ever submitted, in any of its channels. Autocomplete offers these
// so a credit block written for one anime can be reused on the next without being retyped.
pub fn history_path(server_id: u64) -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("attribute_history.json")
}

pub async fn read_list(path: PathBuf) -> Vec<String> {
    let Ok(raw) = tokio::fs::read_to_string(&path).await else {
        return Vec::new();
    };
    serde_json::from_str::<Vec<String>>(&raw).unwrap_or_default()
}

pub async fn write_list(path: PathBuf, list: &[String]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    let raw = serde_json::to_string_pretty(list).map_err(|e| e.to_string())?;
    tokio::fs::write(&path, raw).await.map_err(|e| e.to_string())
}

// Adds to a list unless it is already there. The same credit block is submitted again every time
// somebody picks it out of autocomplete, and a list that grew a copy each time would fill the
// twenty-five choices Discord shows with one line repeated.
pub fn add_once(list: &mut Vec<String>, value: &str) -> bool {
    if list.iter().any(|existing| existing == value) {
        return false;
    }
    list.push(value.to_string());
    true
}

pub async fn read_styles(server_id: u64, channel_id: u64) -> Option<String> {
    let bytes = tokio::fs::read(styles_path(server_id, channel_id)).await.ok()?;
    let text = String::from_utf8_lossy(&bytes).to_string();
    (!text.trim().is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MERGED: &str = "[Script Info]\nTitle: Default file\nPlayResX: 1280\nPlayResY: 720\nWrapStyle: 0\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Arial,48\nStyle: Sign,Arial,30\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:01.00,0:00:02.00,Default,,0,0,0,,Hello, world\nDialogue: 0,0:00:03.00,0:00:04.00,Sign,,0,0,0,,{\\pos(1,2)}Sign\n";

    const STYLES: &str = "[Script Info]\nPlayResX: 1920\nPlayResY: 1080\nScaledBorderAndShadow: yes\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize\nStyle: Default,Gandhi Sans,72\nStyle: Credits,Gandhi Sans,54\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:01.00,Default,,0,0,0,,sample the styles were drawn against\n";

    #[test]
    fn a_dialogue_is_stamped_and_accepted_with_or_without_its_prefix() {
        let stamped = "Dialogue: 0,0:00:05.00,0:00:15.00,Credits,PandoraIdentifier,0,0,0,,Çeviri: %tl%";
        assert_eq!(
            normalize_dialogue("Dialogue: 0,0:00:05.00,0:00:15.00,Credits,,0,0,0,,Çeviri: %tl%").unwrap(),
            stamped
        );
        assert_eq!(
            normalize_dialogue("  0,0:00:05.00,0:00:15.00,Credits,SomeoneElse,0,0,0,,Çeviri: %tl%  ").unwrap(),
            stamped
        );
        // Commas in the text belong to the text.
        assert!(normalize_dialogue("Dialogue: 0,0:00:05.00,0:00:15.00,Credits,,0,0,0,,a, b, c")
            .unwrap()
            .ends_with(",a, b, c"));
    }

    #[test]
    fn a_line_that_could_not_render_is_refused_with_a_reason() {
        for (line, expected) in [
            ("", "cannot be empty"),
            ("Dialogue: 0,0:00:05.00,Credits,,0,0,0,,short", "10 comma-separated fields"),
            ("Dialogue: 0,five,0:00:15.00,Credits,,0,0,0,,text", "not an ASS timestamp"),
            ("Dialogue: 0,0:00:05.00,0:00:15.00,Credits,,0,0,0,,   ", "would render nothing"),
        ] {
            let error = normalize_dialogue(line).unwrap_err();
            assert!(error.contains(expected), "{} -> {}", line, error);
        }
    }

    #[test]
    fn only_named_variables_are_substituted() {
        let pairs = [("tl", "Myisha".to_string()), ("ts", "Beatrice".to_string())];
        assert_eq!(
            substitute("TL %tl% / TS %ts% / ENC %enc%", &pairs),
            "TL Myisha / TS Beatrice / ENC %enc%"
        );
    }

    #[test]
    fn styles_are_replaced_along_with_the_canvas_they_were_drawn_for() {
        let styled = replace_styles(MERGED, STYLES).unwrap();
        assert!(styled.text.contains("Style: Default,Gandhi Sans,72"));
        assert!(styled.text.contains("Style: Credits,Gandhi Sans,54"));
        assert!(!styled.text.contains("Style: Sign,Arial,30"));
        assert!(styled.text.contains("PlayResX: 1920"));
        assert!(styled.text.contains("PlayResY: 1080"));
        assert!(styled.text.contains("ScaledBorderAndShadow: yes"));
        // The server owns the wrap style, and the events are the merge's own.
        assert!(styled.text.contains("WrapStyle: 0"));
        assert!(styled.text.contains("Hello, world"));
        // The attribute file's sample line is not carried over with its styles.
        assert!(!styled.text.contains("sample the styles were drawn against"));
    }

    #[test]
    fn events_left_without_a_style_are_reported_rather_than_rewritten() {
        let styled = replace_styles(MERGED, STYLES).unwrap();
        assert_eq!(styled.missing_styles, vec!["Sign".to_string()]);
        assert!(styled.text.contains("Dialogue: 0,0:00:03.00,0:00:04.00,Sign,"));
        assert!(replace_styles(MERGED, "[Script Info]\nTitle: no styles here\n").is_err());
    }

    #[test]
    fn injected_lines_replace_the_previous_ones_instead_of_stacking() {
        let first = inject_dialogues(
            MERGED,
            &[normalize_dialogue("Dialogue: 0,0:00:05.00,0:00:15.00,Credits,,0,0,0,,TL: Myisha").unwrap()],
        );
        assert_eq!(first.matches("PandoraIdentifier").count(), 1);
        let second = inject_dialogues(
            &first,
            &[normalize_dialogue("Dialogue: 0,0:00:05.00,0:00:15.00,Credits,,0,0,0,,TL: Beatrice").unwrap()],
        );
        assert_eq!(second.matches("PandoraIdentifier").count(), 1);
        assert!(second.contains("TL: Beatrice"));
        assert!(!second.contains("TL: Myisha"));
        // The merge's own events are untouched by either pass.
        assert!(second.contains("Hello, world"));
        assert_eq!(second.lines().filter(|l| l.starts_with("Dialogue:")).count(), 3);
        // And an empty list takes the credits back out.
        assert!(!inject_dialogues(&second, &[]).contains("PandoraIdentifier"));
    }

    #[test]
    fn injection_lands_inside_the_events_section() {
        let script = format!("{}\n[Aegisub Project Garbage]\nLast Style Storage: Default\n", MERGED);
        let line = normalize_dialogue("Dialogue: 0,0:00:05.00,0:00:15.00,Credits,,0,0,0,,credits").unwrap();
        let injected = inject_dialogues(&script, &[line.clone()]);
        let lines: Vec<&str> = injected.lines().collect();
        let events = lines.iter().position(|l| *l == "[Events]").unwrap();
        let garbage = lines.iter().position(|l| *l == "[Aegisub Project Garbage]").unwrap();
        let credits = lines.iter().position(|l| *l == line).unwrap();
        assert!(events < credits && credits < garbage);
    }

    #[test]
    fn crlf_scripts_stay_crlf() {
        let crlf = MERGED.replace('\n', "\r\n");
        let line = normalize_dialogue("Dialogue: 0,0:00:05.00,0:00:15.00,Credits,,0,0,0,,credits").unwrap();
        let injected = inject_dialogues(&crlf, &[line]);
        assert!(injected.contains("\r\n"));
        assert!(!injected.replace("\r\n", "").contains('\n'));
        assert!(replace_styles(&crlf, STYLES).unwrap().text.contains("\r\n"));
    }

    #[test]
    fn a_file_that_defines_no_styles_is_told_apart_from_one_that_does() {
        assert_eq!(style_names(STYLES), vec!["Default".to_string(), "Credits".to_string()]);
        assert!(style_names("[Script Info]\nTitle: nothing here\n").is_empty());
        assert!(style_names("[V4+ Styles]\nFormat: Name, Fontname\n").is_empty());
    }

    #[test]
    fn a_dialogue_is_remembered_once_however_often_it_is_submitted() {
        let mut list = Vec::new();
        assert!(add_once(&mut list, "Dialogue: a"));
        assert!(!add_once(&mut list, "Dialogue: a"));
        assert!(add_once(&mut list, "Dialogue: b"));
        assert_eq!(list, vec!["Dialogue: a".to_string(), "Dialogue: b".to_string()]);
    }
}
