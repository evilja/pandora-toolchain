use std::path::PathBuf;

// Positional indexes of server-level upload settings in `meta.pandora`.
pub const SERVER_DRIVE_ONLY_LINE: usize = 14;
pub const SERVER_HLS_LINE: usize = 17;

// Every distribution site names its fansubs differently — AnimeciX addresses a numeric translator
// template, OpenAnime a `fansubSecureName` string, Anizm a numeric staff-form fansub id — so each
// one keeps its own selection line instead of sharing a single "fansub name" value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FansubSite {
    AnimeciX,
    OpenAnime,
    Anizm,
}

impl FansubSite {
    pub const ALL: [FansubSite; 3] = [Self::AnimeciX, Self::OpenAnime, Self::Anizm];

    pub fn label(self) -> &'static str {
        match self {
            Self::AnimeciX => "AnimeciX",
            Self::OpenAnime => "OpenAnime",
            Self::Anizm => "Anizm",
        }
    }

    // Discord option name on `/edit`, also used as the autocomplete focus key.
    pub fn option_name(self) -> &'static str {
        match self {
            Self::AnimeciX => "animecix_fansub",
            Self::OpenAnime => "openanime_fansub",
            Self::Anizm => "anizm_fansub",
        }
    }

    pub fn meta_line(self) -> usize {
        match self {
            Self::AnimeciX => 13,
            Self::OpenAnime => 15,
            Self::Anizm => 16,
        }
    }

    pub fn from_option_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|site| site.option_name() == name)
    }
}

pub fn fansub_from_meta(meta: &str, site: FansubSite) -> Option<String> {
    meta.lines()
        .nth(site.meta_line())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn read_server_fansub(server_id: u64, site: FansubSite) -> Option<String> {
    let meta = std::fs::read_to_string(server_meta_path(server_id)).ok()?;
    fansub_from_meta(&meta, site)
}

pub fn server_meta_path(server_id: u64) -> PathBuf {
    PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("meta.pandora")
}

// Missing and explicitly disabled values preserve the legacy multi-provider behavior. Unknown
// non-empty values fail closed to Drive-only so a damaged restrictive policy cannot silently
// re-enable public streaming-host uploads.
pub fn drive_only_from_meta(meta: &str) -> bool {
    let Some(value) = meta.lines().nth(SERVER_DRIVE_ONLY_LINE).map(str::trim) else {
        return false;
    };
    match value.to_ascii_lowercase().as_str() {
        "" | "false" | "0" | "disabled" | "off" => false,
        "true" | "1" | "enabled" | "on" => true,
        _ => true,
    }
}

pub async fn server_drive_only(server_id: Option<u64>) -> bool {
    let Some(server_id) = server_id else {
        return false;
    };
    tokio::fs::read_to_string(server_meta_path(server_id))
        .await
        .map(|meta| drive_only_from_meta(&meta))
        .unwrap_or(false)
}

pub fn hls_from_meta(meta: &str) -> bool {
    matches!(
        meta.lines()
            .nth(SERVER_HLS_LINE)
            .map(str::trim)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "true" | "1" | "enabled" | "on"
    )
}

pub async fn server_hls_enabled(server_id: Option<u64>) -> bool {
    let Some(server_id) = server_id else {
        return false;
    };
    tokio::fs::read_to_string(server_meta_path(server_id))
        .await
        .map(|meta| hls_from_meta(&meta))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta_with_policy(value: Option<&str>) -> String {
        let mut lines = vec![String::new(); SERVER_DRIVE_ONLY_LINE];
        if let Some(value) = value {
            lines.push(value.to_string());
        }
        lines.join("\n")
    }

    #[test]
    fn missing_or_disabled_policy_preserves_multi_provider_uploads() {
        assert!(!drive_only_from_meta(&meta_with_policy(None)));
        for value in ["", "false", "0", "disabled", "OFF"] {
            assert!(!drive_only_from_meta(&meta_with_policy(Some(value))));
        }
    }

    #[test]
    fn enabled_policy_is_drive_only() {
        for value in ["true", "1", "enabled", "ON"] {
            assert!(drive_only_from_meta(&meta_with_policy(Some(value))));
        }
    }

    #[test]
    fn malformed_restrictive_policy_fails_closed() {
        assert!(drive_only_from_meta(&meta_with_policy(Some("tru"))));
    }

    #[test]
    fn hls_is_opt_in_on_its_own_line() {
        let mut lines = vec![String::new(); SERVER_HLS_LINE + 1];
        assert!(!hls_from_meta(&lines.join("\n")));
        for enabled in ["true", "1", "enabled", "ON"] {
            lines[SERVER_HLS_LINE] = enabled.to_string();
            assert!(hls_from_meta(&lines.join("\n")));
        }
        for disabled in ["", "false", "0", "tru"] {
            lines[SERVER_HLS_LINE] = disabled.to_string();
            assert!(!hls_from_meta(&lines.join("\n")));
        }
    }

    #[test]
    fn every_site_reads_its_own_selection_line() {
        let mut lines = vec![String::new(); 17];
        lines[13] = "218".to_string();
        lines[15] = "  akira-subs  ".to_string();
        lines[16] = "42".to_string();
        let meta = lines.join("\n");

        assert_eq!(
            fansub_from_meta(&meta, FansubSite::AnimeciX).as_deref(),
            Some("218")
        );
        assert_eq!(
            fansub_from_meta(&meta, FansubSite::OpenAnime).as_deref(),
            Some("akira-subs")
        );
        assert_eq!(fansub_from_meta(&meta, FansubSite::Anizm).as_deref(), Some("42"));
    }

    #[test]
    fn blank_and_missing_selection_lines_are_unset() {
        let short = (0..14).map(|_| String::new()).collect::<Vec<_>>().join("\n");
        for site in FansubSite::ALL {
            assert_eq!(fansub_from_meta(&short, site), None);
            assert_eq!(fansub_from_meta("", site), None);
        }
    }

    #[test]
    fn site_lines_and_option_names_are_distinct() {
        for site in FansubSite::ALL {
            assert_eq!(FansubSite::from_option_name(site.option_name()), Some(site));
            assert_ne!(site.meta_line(), SERVER_DRIVE_ONLY_LINE);
            assert_ne!(site.meta_line(), SERVER_HLS_LINE);
        }
        assert_eq!(FansubSite::from_option_name("concat"), None);
    }
}
