use std::path::Path;

use crate::lib::env::standard::BUILD_PATH;

// What revision a machine is running, as one small file next to the rest of the environment.
//
// `CARGO_PKG_VERSION` alone cannot answer the question a cluster needs answered — it changes when
// somebody edits Cargo.toml, not when a deploy happens — so a gitsync that moves HEAD bumps a
// counter beside it. The counter is what a node compares; the commit is what it pulls. Both are
// persisted because a gitsync ends in `exit(0)`: the number has to survive the restart it causes,
// and be correct by the time the API answers again.
//
// A build number is meaningless across machines except as "the coordinator's". A node records the
// coordinator's number verbatim once it has landed on the coordinator's commit, so the pair only
// ever means "this node is level with the coordinator as of build N".

const HEADER: &str = "PNBUILD1";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReleaseRecord {
    pub build: u64,
    pub commit: String,
}

impl ReleaseRecord {
    // An absent file reads as build 0 with no commit, which every advertised release differs from.
    // That is deliberate: a machine that has never recorded a build should sync rather than assume
    // it is current.
    pub fn is_level_with(&self, build: u64, commit: &str) -> bool {
        self.build == build && !self.commit.is_empty() && self.commit == commit
    }
}

pub fn read() -> ReleaseRecord {
    read_from(Path::new(BUILD_PATH))
}

pub fn read_from(path: &Path) -> ReleaseRecord {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return ReleaseRecord::default();
    };
    parse(&contents)
}

fn parse(contents: &str) -> ReleaseRecord {
    let mut lines = contents.lines();
    // A file whose header is not ours is not guessed at: an unreadable record means "unknown", and
    // unknown syncs.
    if lines.next().map(str::trim) != Some(HEADER) {
        return ReleaseRecord::default();
    }
    let build = lines
        .next()
        .and_then(|line| line.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let commit = lines.next().unwrap_or("").trim().to_string();
    ReleaseRecord { build, commit }
}

fn render(record: &ReleaseRecord) -> String {
    format!("{}\n{}\n{}\n", HEADER, record.build, record.commit)
}

pub fn write(record: &ReleaseRecord) -> std::io::Result<()> {
    write_to(Path::new(BUILD_PATH), record)
}

pub fn write_to(path: &Path, record: &ReleaseRecord) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, render(record))
}

// The coordinator's side of a gitsync that moved HEAD. Bumping and writing are one call because a
// number that is incremented but never persisted is worse than one that never moved: the cluster
// would be told to update to a build the coordinator forgets on its next restart.
pub fn bump(commit: &str) -> ReleaseRecord {
    let mut record = read();
    record.build += 1;
    record.commit = commit.to_string();
    if let Err(error) = write(&record) {
        eprintln!("[Pandora] could not record build {}: {}", record.build, error);
    }
    record
}

// The node's side. It adopts the coordinator's number rather than counting its own, so the two are
// comparable at all; recording it is what stops the next poll asking for the same update again.
pub fn adopt(build: u64, commit: &str) -> ReleaseRecord {
    let record = ReleaseRecord {
        build,
        commit: commit.to_string(),
    };
    if let Err(error) = write(&record) {
        eprintln!("[link] could not record build {}: {}", build, error);
    }
    record
}

// A commit id shortened for a log line or a Discord reply. By characters rather than by bytes: the
// value arrives over the wire from another machine, and a byte slice through a multibyte character
// panics — which is not a way for a status line to end a process.
pub fn short_commit(commit: &str, width: usize) -> String {
    commit.chars().take(width).collect()
}

// The checkout this process was built from. Under Docker the repository is mounted somewhere other
// than the working directory — `DB/` sits beside the binary, the source does not — so the two are
// not interchangeable and every caller has to ask the same way.
pub fn repo_path() -> String {
    std::env::var("PANDORA_GITSYNC_REPO").unwrap_or_else(|_| {
        std::env::current_dir()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| ".".to_string())
    })
}

// How a Pandora process hands itself over to the build it just pulled.
//
// There is no in-place upgrade: the binary that pulled the source is the old one, and the only
// thing that can run the new source is a new process. Under `start.sh` and `start.bat` that is the
// restart loop, which rebuilds before it runs. Under Docker the image itself has to be rebuilt, so
// a request file is left for the watcher on the host and this process waits to be killed rather
// than exiting into a container restart that would come up on the same old image.
pub async fn restart_into_new_build() -> ! {
    let rebuild_requested = match std::env::var("PANDORA_GITSYNC_REQUEST") {
        Ok(request_path) => {
            let request_path = std::path::PathBuf::from(request_path);
            if let Some(parent) = request_path.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            tokio::fs::write(request_path, b"rebuild
").await.is_ok()
        }
        Err(_) => false,
    };
    if rebuild_requested {
        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
    } else {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    std::process::exit(0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pandora-release-{}-{}",
            name,
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("build.pandora")
    }

    #[test]
    fn a_record_round_trips_through_the_file() {
        let path = temp("roundtrip");
        let record = ReleaseRecord {
            build: 42,
            commit: "17ff1685".to_string(),
        };
        write_to(&path, &record).unwrap();
        assert_eq!(read_from(&path), record);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    // A machine that has never recorded a build must sync rather than assume it is current, so an
    // absent or unreadable file reads as a record nothing is level with.
    #[test]
    fn an_absent_or_foreign_file_is_level_with_nothing() {
        let path = temp("absent");
        assert_eq!(read_from(&path), ReleaseRecord::default());
        std::fs::write(&path, "something else\n7\nabc\n").unwrap();
        let record = read_from(&path);
        assert_eq!(record, ReleaseRecord::default());
        assert!(!record.is_level_with(0, ""));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    // Matching the number is not enough: two machines can hold the same build having pulled
    // different commits if one of them failed to land the checkout it recorded.
    #[test]
    fn being_level_needs_the_commit_as_well_as_the_number() {
        let record = ReleaseRecord {
            build: 3,
            commit: "aaaa".to_string(),
        };
        assert!(record.is_level_with(3, "aaaa"));
        assert!(!record.is_level_with(3, "bbbb"));
        assert!(!record.is_level_with(4, "aaaa"));
    }
}
