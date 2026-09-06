use std::path::PathBuf;

// What a person is credited as, wherever Pandora writes a name into a release. One alias per
// Discord account across every server: a translator is the same person in all of them, and a name
// that had to be set again in each guild would be wrong in whichever one was forgotten.
//
// `<user id>|<name>` per line, the same shape as every other `.pandora` list.
pub fn alias_path() -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join("global")
        .join("environment")
        .join("aliases.pandora")
}

pub fn parse_aliases(contents: &str) -> Vec<(u64, String)> {
    contents
        .lines()
        .filter_map(|line| {
            let (id, name) = line.trim().split_once('|')?;
            let id = id.trim().parse::<u64>().ok()?;
            let name = name.trim();
            (!name.is_empty()).then(|| (id, name.to_string()))
        })
        .collect()
}

pub fn render_aliases(aliases: &[(u64, String)]) -> String {
    let mut out = aliases
        .iter()
        .map(|(id, name)| format!("{}|{}", id, name))
        .collect::<Vec<_>>()
        .join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

// Setting an empty name removes the entry rather than storing a blank one, which is what makes
// `/alias choose name:-` fall back to the Discord display name again.
pub fn set_alias(aliases: &mut Vec<(u64, String)>, user_id: u64, name: &str) -> bool {
    let name = name.trim();
    let existing = aliases.iter().position(|(id, _)| *id == user_id);
    if name.is_empty() || name == "-" {
        return match existing {
            Some(index) => {
                aliases.remove(index);
                true
            }
            None => false,
        };
    }
    match existing {
        Some(index) => {
            if aliases[index].1 == name {
                return false;
            }
            aliases[index].1 = name.to_string();
        }
        None => aliases.push((user_id, name.to_string())),
    }
    true
}

pub async fn read_aliases() -> Vec<(u64, String)> {
    tokio::fs::read_to_string(alias_path())
        .await
        .map(|contents| parse_aliases(&contents))
        .unwrap_or_default()
}

pub async fn write_aliases(aliases: &[(u64, String)]) -> Result<(), String> {
    let path = alias_path();
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&path, render_aliases(aliases))
        .await
        .map_err(|e| e.to_string())
}

pub async fn alias_for(user_id: u64) -> Option<String> {
    read_aliases()
        .await
        .into_iter()
        .find(|(id, _)| *id == user_id)
        .map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_list_round_trips_and_junk_lines_are_dropped() {
        let stored = "1|Myisha\n\n2 | Beatrice \nnot a line\n3|\n";
        let aliases = parse_aliases(stored);
        assert_eq!(
            aliases,
            vec![(1, "Myisha".to_string()), (2, "Beatrice".to_string())]
        );
        assert_eq!(render_aliases(&aliases), "1|Myisha\n2|Beatrice\n");
        assert_eq!(render_aliases(&[]), "");
    }

    #[test]
    fn setting_replaces_and_clearing_removes() {
        let mut aliases = vec![(1, "Myisha".to_string())];
        assert!(set_alias(&mut aliases, 1, "Evilja"));
        assert_eq!(aliases, vec![(1, "Evilja".to_string())]);
        // Nothing changed, so nothing is written back.
        assert!(!set_alias(&mut aliases, 1, "Evilja"));
        assert!(set_alias(&mut aliases, 2, "  Beatrice  "));
        assert_eq!(aliases[1], (2, "Beatrice".to_string()));
        assert!(set_alias(&mut aliases, 1, "-"));
        assert_eq!(aliases, vec![(2, "Beatrice".to_string())]);
        assert!(!set_alias(&mut aliases, 1, ""));
    }

    // A name with the separator in it would come back as a different name, or as nothing.
    #[test]
    fn a_name_carrying_the_separator_survives_the_file() {
        let mut aliases = Vec::new();
        set_alias(&mut aliases, 7, "a|b");
        let reparsed = parse_aliases(&render_aliases(&aliases));
        assert_eq!(reparsed, vec![(7, "a|b".to_string())]);
    }
}
