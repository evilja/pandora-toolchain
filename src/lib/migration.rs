use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::lib::env::standard::{MIGRATION_DIR, MIGRATION_LEDGER_PATH};

// On-disk changes a new revision needs, kept out of the Rust that would otherwise have to carry
// them forever. A migration is a pair of scripts in `migration/` — one `.sh`, one `.ps1` — and
// gitsync runs whichever of the two this platform can, after the pull and before the restart. The
// scripts are therefore the newly pulled ones while the binary is still the old one, which is the
// order that makes sense: a migration prepares the state the binary about to be built expects.
//
// Ordering is by an id in a header comment, not by the filesystem's clock and not by the deployed
// machine's:
//
//     # pandora-migration: 1756598400
//
// The value is a unix time only so that two people writing migrations on the same day cannot pick
// the same number. Nothing ever compares it against `now()` — it is an identifier that goes
// forward, and the only comparison is against the highest one this machine has already run.

const HEADER: &str = "PNMIGRATION1";
const MARKER: &str = "# pandora-migration:";
// How far into a script to look for the marker, so a file that is not a migration at all is
// rejected by reading a few lines rather than all of it.
const MARKER_SCAN_LINES: usize = 20;
// A migration that never returns would hold a gitsync open forever, and a gitsync holds the whole
// bot: the shrine is already dead by the time this runs.
const SCRIPT_TIMEOUT_SECS: u64 = 600;

#[cfg(unix)]
const SCRIPT_EXT: &str = "sh";
#[cfg(not(unix))]
const SCRIPT_EXT: &str = "ps1";

#[cfg(unix)]
const COUNTERPART_EXT: &str = "ps1";
#[cfg(not(unix))]
const COUNTERPART_EXT: &str = "sh";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    pub id: u64,
    // The file stem, which is what an operator sees in a report and what pairs the two platforms'
    // copies of one migration.
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationFailure {
    pub id: u64,
    pub name: String,
    pub error: String,
}

impl MigrationFailure {
    pub fn line(&self) -> String {
        format!("{} {}: {}", self.id, self.name, self.error)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Ledger {
    // The highest id that has run here successfully.
    pub last_id: u64,
    // The migration that stopped the last run, kept so a node can report it upstream and an
    // operator can see it on `/lsnode` rather than only in a log nobody opened.
    pub failure: Option<MigrationFailure>,
}

#[derive(Clone, Debug, Default)]
pub struct MigrationRun {
    pub ran: Vec<String>,
    pub failed: Option<MigrationFailure>,
}

impl MigrationRun {
    // One line for a Discord reply or a node's startup log. Silent when a sync had nothing to run,
    // which is the ordinary case and does not deserve a line.
    pub fn summary(&self) -> Option<String> {
        match (&self.failed, self.ran.len()) {
            (Some(failure), 0) => Some(format!("Migration `{}` failed: {}", failure.name, failure.error)),
            (Some(failure), n) => Some(format!(
                "Ran {} migration(s); `{}` failed: {}",
                n, failure.name, failure.error
            )),
            (None, 0) => None,
            (None, n) => Some(format!("Ran {} migration(s): {}", n, self.ran.join(", "))),
        }
    }
}

// The id in a script's header. A file without one is not a migration — a README or a helper the
// scripts source — and is skipped rather than guessed at.
fn marker_id(contents: &str) -> Option<u64> {
    for line in contents.lines().take(MARKER_SCAN_LINES) {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(MARKER) {
            return rest.trim().parse::<u64>().ok();
        }
    }
    None
}

// Every migration this repository carries for this platform, lowest id first. A pair that is
// missing this platform's half is reported and skipped: refusing to start over it would strand a
// deployment on a mistake it cannot fix without editing the repository.
pub fn discover(repo: &Path) -> Vec<Migration> {
    let dir = repo.join(MIGRATION_DIR);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut found: Vec<Migration> = Vec::new();
    let mut stems: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if extension == SCRIPT_EXT || extension == COUNTERPART_EXT {
            let stem = stem.to_string();
            if !stems.contains(&stem) {
                stems.push(stem);
            }
        }
        if extension != SCRIPT_EXT {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(id) = marker_id(&contents) else {
            continue;
        };
        found.push(Migration {
            id,
            name: path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string(),
            path,
        });
    }
    // A migration is written twice so either platform can deploy it. A missing half is only a
    // problem for the platform that is missing, so it is a warning here and a skip there.
    for stem in &stems {
        if !dir.join(format!("{stem}.{COUNTERPART_EXT}")).exists() {
            eprintln!("[Pandora] migration {stem} has no .{COUNTERPART_EXT} counterpart");
        }
        if !dir.join(format!("{stem}.{SCRIPT_EXT}")).exists() {
            eprintln!("[Pandora] migration {stem} has no .{SCRIPT_EXT} half and cannot run here");
        }
    }
    found.sort_by(|a, b| a.id.cmp(&b.id).then(a.name.cmp(&b.name)));
    found
}

pub fn read_ledger() -> Ledger {
    read_ledger_from(Path::new(MIGRATION_LEDGER_PATH))
}

pub fn read_ledger_from(path: &Path) -> Ledger {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Ledger::default();
    };
    parse_ledger(&contents)
}

fn parse_ledger(contents: &str) -> Ledger {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) != Some(HEADER) {
        return Ledger::default();
    }
    let last_id = lines
        .next()
        .and_then(|line| line.trim().parse::<u64>().ok())
        .unwrap_or(0);
    let failure = lines
        .next()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .and_then(parse_failure);
    Ledger { last_id, failure }
}

fn parse_failure(line: &str) -> Option<MigrationFailure> {
    let (id, rest) = line.split_once(' ')?;
    let id = id.trim().parse::<u64>().ok()?;
    let (name, error) = rest.split_once(':')?;
    Some(MigrationFailure {
        id,
        name: name.trim().to_string(),
        error: error.trim().to_string(),
    })
}

fn render_ledger(ledger: &Ledger) -> String {
    let failure = ledger
        .failure
        .as_ref()
        .map(MigrationFailure::line)
        .unwrap_or_default();
    format!("{}\n{}\n{}\n", HEADER, ledger.last_id, failure)
}

pub fn write_ledger(ledger: &Ledger) -> std::io::Result<()> {
    write_ledger_to(Path::new(MIGRATION_LEDGER_PATH), ledger)
}

pub fn write_ledger_to(path: &Path, ledger: &Ledger) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, render_ledger(ledger))
}

// A fresh install is already in the current format — `--setup` just wrote it — so it records every
// migration as done without running any. An existing deployment that predates the ledger has no
// such guarantee and must run them all, which is why this is called from setup and not from the
// first sync that finds no ledger.
pub fn seed(repo: &Path) {
    if Path::new(MIGRATION_LEDGER_PATH).exists() {
        return;
    }
    let last_id = discover(repo).last().map(|entry| entry.id).unwrap_or(0);
    if let Err(error) = write_ledger(&Ledger { last_id, failure: None }) {
        eprintln!("[Pandora] could not seed the migration ledger: {error}");
        return;
    }
    println!("[Pandora] new install: migrations up to {last_id} recorded without running");
}

// Runs every migration this machine has not run, lowest id first, stopping at the first failure.
//
// The ledger advances per script rather than at the end, so a run that dies halfway keeps what it
// achieved. A failure leaves the ledger below the script that failed, which is what makes the next
// sync retry it — and leaves the reason on the ledger, which is what lets a node report it.
pub async fn run_pending(repo: &Path) -> MigrationRun {
    let mut ledger = read_ledger();
    let pending = discover(repo)
        .into_iter()
        .filter(|entry| entry.id > ledger.last_id)
        .collect::<Vec<_>>();
    let mut run = MigrationRun::default();
    if pending.is_empty() {
        // Nothing to do, and nothing outstanding: a stale failure from a script that has since
        // been removed must not keep flying a red flag on `/lsnode`.
        if ledger.failure.is_some() {
            ledger.failure = None;
            let _ = write_ledger(&ledger);
        }
        return run;
    }
    for entry in pending {
        println!("[Pandora] running migration {} ({})", entry.name, entry.id);
        match execute(&entry).await {
            Ok(()) => {
                ledger.last_id = entry.id;
                ledger.failure = None;
                if let Err(error) = write_ledger(&ledger) {
                    // The script ran; not recording it means it runs again next time. Say so
                    // rather than letting a silent write failure look like a script that loops.
                    eprintln!("[Pandora] migration {} ran but was not recorded: {}", entry.name, error);
                }
                run.ran.push(entry.name.clone());
            }
            Err(error) => {
                eprintln!("[Pandora] migration {} failed: {}", entry.name, error);
                let failure = MigrationFailure {
                    id: entry.id,
                    name: entry.name.clone(),
                    error,
                };
                ledger.failure = Some(failure.clone());
                let _ = write_ledger(&ledger);
                run.failed = Some(failure);
                break;
            }
        }
    }
    run
}

// Scripts run from the process's own working directory, not from the repository: they operate on
// `DB/`, which is a mounted volume beside the binary and, under Docker, not inside the checkout at
// all. The repository reaches them as `PANDORA_REPO` for the cases that need a file from it.
async fn execute(entry: &Migration) -> Result<(), String> {
    let mut command = script_command(&entry.path);
    command.env("PANDORA_REPO", entry.path.parent().and_then(|p| p.parent()).unwrap_or(Path::new(".")));
    command.env("PANDORA_MIGRATION_ID", entry.id.to_string());
    command.kill_on_drop(true);
    let output = tokio::time::timeout(
        Duration::from_secs(SCRIPT_TIMEOUT_SECS),
        command.output(),
    )
    .await
    .map_err(|_| format!("timed out after {SCRIPT_TIMEOUT_SECS}s"))?
    .map_err(|error| format!("could not be started: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        println!("[Pandora] {} | {}", entry.name, line);
    }
    if output.status.success() {
        return Ok(());
    }
    // The last line of stderr is what a shell script's `echo >&2 ... ; exit 1` leaves behind, and
    // it is the part worth carrying into a Discord reply or a node's report.
    let stderr = String::from_utf8_lossy(&output.stderr);
    for line in stderr.lines() {
        eprintln!("[Pandora] {} | {}", entry.name, line);
    }
    let reason = stderr
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no output");
    Err(format!("exited {} — {}", output.status.code().unwrap_or(-1), reason))
}

#[cfg(unix)]
fn script_command(path: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("sh");
    command.arg(path);
    command
}

#[cfg(not(unix))]
fn script_command(path: &Path) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("powershell");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pandora-migration-{}-{}",
            name,
            std::process::id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(dir.join(MIGRATION_DIR)).unwrap();
        dir
    }

    fn write_pair(repo: &Path, stem: &str, id: u64) {
        for extension in ["sh", "ps1"] {
            std::fs::write(
                repo.join(MIGRATION_DIR).join(format!("{stem}.{extension}")),
                format!("# pandora-migration: {id}\n"),
            )
            .unwrap();
        }
    }

    // The id comes from the header, so a file may be renamed without changing where it sits in the
    // order, and discovery is by id rather than by however the directory happens to sort.
    #[test]
    fn migrations_are_ordered_by_the_id_in_their_header() {
        let repo = temp("order");
        write_pair(&repo, "zzz-late", 200);
        write_pair(&repo, "aaa-early", 100);

        let found = discover(&repo);
        assert_eq!(
            found.iter().map(|entry| entry.id).collect::<Vec<_>>(),
            vec![100, 200]
        );
        assert_eq!(found[0].name, "aaa-early");
        std::fs::remove_dir_all(&repo).ok();
    }

    // A helper or a README dropped in the directory is not a migration and must not be run as one.
    #[test]
    fn a_file_without_the_marker_is_not_a_migration() {
        let repo = temp("marker");
        std::fs::write(repo.join(MIGRATION_DIR).join("helpers.sh"), "echo hello\n").unwrap();
        assert!(discover(&repo).is_empty());
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn a_ledger_round_trips_with_and_without_a_failure() {
        let path = temp("ledger").join("migration.pandora");
        write_ledger_to(&path, &Ledger { last_id: 5, failure: None }).unwrap();
        let ledger = read_ledger_from(&path);
        assert_eq!(ledger.last_id, 5);
        assert!(ledger.failure.is_none());

        let failure = MigrationFailure {
            id: 9,
            name: "add-purpose".to_string(),
            error: "exited 1 — sed: no such file".to_string(),
        };
        write_ledger_to(&path, &Ledger { last_id: 5, failure: Some(failure.clone()) }).unwrap();
        assert_eq!(read_ledger_from(&path).failure, Some(failure));
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    // An unreadable ledger reads as "nothing has run", which re-runs migrations rather than
    // skipping them. Re-running a migration is survivable; skipping one silently is not.
    #[test]
    fn an_unreadable_ledger_reads_as_nothing_having_run() {
        let path = temp("foreign").join("migration.pandora");
        std::fs::write(&path, "NOTOURS\n9\n").unwrap();
        assert_eq!(read_ledger_from(&path).last_id, 0);
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn a_run_that_did_nothing_reports_nothing() {
        assert!(MigrationRun::default().summary().is_none());
        let failed = MigrationRun {
            ran: Vec::new(),
            failed: Some(MigrationFailure {
                id: 1,
                name: "one".to_string(),
                error: "boom".to_string(),
            }),
        };
        assert_eq!(
            failed.summary().unwrap(),
            "Migration `one` failed: boom"
        );
    }
}
