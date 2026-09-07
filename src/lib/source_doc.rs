// `SOURCE.md` in an episode folder records where that episode is encoded from. It has always been
// one line — `# <link>` — and that is enough for a single-episode torrent, but a `/probe` result is
// two facts and not one: the torrent, and which file inside it is the episode. The second fact
// rides on a `;` comment line, which every reader of this file has always skipped, so a repo
// written by this version still parses under the one before it.

use std::fmt::Write as _;

const PROBE_PREFIX: &str = "; pandora-probe";

// The probe a source link was picked out of. The job id is kept beside the index because the
// worker adopts the probe's own torrent data when it is still in the job DB, which saves a second
// fetch of a torrent Pandora already has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProbeRef {
    pub job_id: u64,
    pub file_index: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceDoc {
    pub link: String,
    pub probe: Option<ProbeRef>,
}

pub fn compose(link: &str, probe: Option<ProbeRef>) -> String {
    let mut out = format!("# {}\n", link.trim());
    if let Some(probe) = probe {
        let _ = writeln!(
            out,
            "{} job={} index={}",
            PROBE_PREFIX, probe.job_id, probe.file_index
        );
    }
    out
}

// The link is the first line that is neither blank nor a comment, with a leading `#` taken off.
// A file with no such line has nothing to encode from and is reported as unparseable by the
// callers, so `None` is the answer rather than an empty link.
pub fn parse(text: &str) -> Option<SourceDoc> {
    let link = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with(';'))
        .map(|line| line.trim_start_matches('#').trim().to_string())
        .filter(|line| !line.is_empty())?;
    Some(SourceDoc {
        link,
        probe: parse_probe(text),
    })
}

// A half-written probe line — one of the two fields missing or unreadable — is no probe at all
// rather than a probe with a guessed index: encoding the wrong file out of a season pack is a
// silent wrong answer, and having none sends the person back to `/source`.
fn parse_probe(text: &str) -> Option<ProbeRef> {
    let rest = text
        .lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(PROBE_PREFIX))?;
    let mut job_id = None;
    let mut file_index = None;
    for field in rest.split_whitespace() {
        match field.split_once('=') {
            Some(("job", value)) => job_id = value.parse::<u64>().ok(),
            Some(("index", value)) => file_index = value.parse::<u64>().ok(),
            _ => {}
        }
    }
    Some(ProbeRef {
        job_id: job_id?,
        file_index: file_index?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_link_composes_and_parses_the_way_it_always_has() {
        let text = compose("https://nyaa.si/view/1234567", None);
        assert_eq!(text, "# https://nyaa.si/view/1234567\n");
        assert_eq!(
            parse(&text),
            Some(SourceDoc {
                link: "https://nyaa.si/view/1234567".to_string(),
                probe: None,
            })
        );
    }

    #[test]
    fn a_probe_survives_the_round_trip() {
        let probe = ProbeRef { job_id: 908123, file_index: 3 };
        let text = compose("https://nyaa.si/view/1234567", Some(probe));
        assert_eq!(
            parse(&text),
            Some(SourceDoc {
                link: "https://nyaa.si/view/1234567".to_string(),
                probe: Some(probe),
            })
        );
    }

    // The point of hanging the probe off a `;` line: a reader that only ever knew about the link
    // reads the same link out of both files.
    #[test]
    fn the_probe_line_is_invisible_to_a_reader_that_only_wants_the_link() {
        let probe = ProbeRef { job_id: 1, file_index: 0 };
        assert_eq!(
            parse(&compose("magnet:?xt=urn:btih:abc", Some(probe))).unwrap().link,
            parse(&compose("magnet:?xt=urn:btih:abc", None)).unwrap().link,
        );
    }

    #[test]
    fn a_probe_line_missing_either_half_is_no_probe() {
        for line in [
            "; pandora-probe job=5",
            "; pandora-probe index=2",
            "; pandora-probe job=five index=2",
            "; pandora-probe",
            "; something else entirely",
        ] {
            let text = format!("# https://nyaa.si/view/1\n{}\n", line);
            assert_eq!(parse(&text).unwrap().probe, None, "{}", line);
        }
    }

    #[test]
    fn a_file_with_no_link_line_parses_to_nothing() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("\n\n"), None);
        assert_eq!(parse("; pandora-probe job=1 index=2\n"), None);
        assert_eq!(parse("#   \n"), None);
    }
}
