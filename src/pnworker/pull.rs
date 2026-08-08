use git2::{
    AutotagOption, Cred, CredentialType, Error, FetchOptions, Oid, RemoteCallbacks, Repository,
};
use std::env;
use std::path::Path;

// A Discord message caps at 2000 characters, so a sync that pulls a long backlog lists only the
// newest commits and counts the rest. The walk itself is bounded too, because a force-push can
// leave the previous tip unreachable, and hiding an unreachable commit walks the whole history.
const MAX_LISTED_COMMITS: usize = 10;
const MAX_SCANNED_COMMITS: usize = 100;

// One revision, so `/gitsync` and `/gitquery` can name what the restart will come up on instead of
// only saying that the pull worked. The title is the commit summary verbatim from git, so it is
// reported unlocalized like any other quoted source text.
pub struct SyncedCommit {
    pub id: String,
    pub title: String,
}

impl SyncedCommit {
    pub fn label(&self) -> String {
        format!("@{} — {}", self.id, self.title)
    }
}

// What a sync moved through: the revision now checked out, plus every commit the pull introduced.
pub struct SyncReport {
    pub head: SyncedCommit,
    // Newest first, matching `git log`. Empty when the pull had nothing to bring in.
    pub new_commits: Vec<SyncedCommit>,
    // The walk hit `MAX_SCANNED_COMMITS`, so `new_commits` is a floor rather than the exact set.
    pub scan_truncated: bool,
}

impl SyncReport {
    pub fn at_head(head: SyncedCommit) -> Self {
        Self {
            head,
            new_commits: Vec::new(),
            scan_truncated: false,
        }
    }

    // An unchanged repository still reports where it is; otherwise the pulled commits are listed
    // and the tip is the first of them, so it is never repeated on its own line.
    pub fn lines(&self) -> Vec<String> {
        if self.new_commits.is_empty() {
            return vec![self.head.label()];
        }
        let mut lines = self
            .new_commits
            .iter()
            .take(MAX_LISTED_COMMITS)
            .map(SyncedCommit::label)
            .collect::<Vec<_>>();
        let remaining = self.new_commits.len().saturating_sub(lines.len());
        if remaining > 0 || self.scan_truncated {
            lines.push(format!(
                "…(+{}{})",
                remaining,
                if self.scan_truncated { "+" } else { "" }
            ));
        }
        lines
    }
}

// Reads HEAD without touching the network, so a failed pull can still report where the bot is.
pub fn head_commit(repo_path: &str) -> Option<SyncedCommit> {
    let repo = Repository::open(repo_path).ok()?;
    Some(read_head(&repo)?.1)
}

fn read_head(repo: &Repository) -> Option<(Oid, SyncedCommit)> {
    let commit = repo.head().ok()?.peel_to_commit().ok()?;
    Some((commit.id(), describe(repo, &commit)))
}

fn describe(repo: &Repository, commit: &git2::Commit) -> SyncedCommit {
    let _ = repo;
    // `short_id` honours the repo's own abbreviation length and grows it when a prefix collides.
    let id = commit
        .as_object()
        .short_id()
        .ok()
        .and_then(|buffer| buffer.as_str().map(str::to_string))
        .unwrap_or_else(|| commit.id().to_string().chars().take(7).collect());
    SyncedCommit {
        id,
        title: commit.summary().unwrap_or("(no commit title)").to_string(),
    }
}

// Commits reachable from `to` but not from `from`, newest first — the same set `git log from..to`
// prints. Hiding is best-effort: after a force-push the old tip is unreachable and nothing is
// hidden, which is why the walk is capped instead of trusting the range to be small.
fn commits_between(repo: &Repository, from: Oid, to: Oid) -> (Vec<SyncedCommit>, bool) {
    let Ok(mut walk) = repo.revwalk() else {
        return (Vec::new(), false);
    };
    if walk.push(to).is_err() {
        return (Vec::new(), false);
    }
    let _ = walk.hide(from);
    let mut commits = Vec::new();
    let mut truncated = false;
    for oid in walk {
        if commits.len() >= MAX_SCANNED_COMMITS {
            truncated = true;
            break;
        }
        let Ok(oid) = oid else { continue };
        if let Ok(commit) = repo.find_commit(oid) {
            commits.push(describe(repo, &commit));
        }
    }
    (commits, truncated)
}

pub fn git_pull(repo_path: &str) -> Result<SyncReport, git2::Error> {
    let repo = Repository::open(repo_path)?;
    let config = repo.config().ok();
    let head = repo.head()?;
    let refname = head.name()
        .ok_or_else(|| git2::Error::from_str("invalid HEAD"))?
        .to_owned();
    let branch = head.shorthand()
        .ok_or_else(|| git2::Error::from_str("invalid branch"))?
        .to_owned();
    // Captured before the fast-forward so the pulled range can be walked afterwards.
    let previous = head.target();
    drop(head);

    let mut remote = repo.find_remote("origin")?;
    let mut fetch_opts = FetchOptions::new();
    fetch_opts.download_tags(AutotagOption::All);
    let mut callbacks = RemoteCallbacks::new();
    callbacks.credentials(move |url, username_from_url, allowed| {
        gitsync_credentials(config.as_ref(), url, username_from_url, allowed)
    });
    fetch_opts.remote_callbacks(callbacks);
    remote.fetch(&[&branch], Some(&mut fetch_opts), None)?;

    let fetch_head = repo.find_reference("FETCH_HEAD")?;
    let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_fast_forward() {
        let mut reference = repo.find_reference(&refname)?;
        reference.set_target(fetch_commit.id(), "Fast-forward")?;
        repo.set_head(&refname)?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
    } else if analysis.is_up_to_date() {
        println!("Already up to date.");
    } else {
        eprintln!("Merge required — fast-forward only supported here.");
    }

    let (current, head) =
        read_head(&repo).ok_or_else(|| git2::Error::from_str("HEAD does not point at a commit"))?;
    let (new_commits, scan_truncated) = match previous {
        Some(previous) if previous != current => commits_between(&repo, previous, current),
        _ => (Vec::new(), false),
    };
    Ok(SyncReport {
        head,
        new_commits,
        scan_truncated,
    })
}

fn gitsync_credentials(
    config: Option<&git2::Config>,
    url: &str,
    username_from_url: Option<&str>,
    allowed: CredentialType,
) -> Result<Cred, Error> {
    let username = env::var("PANDORA_GITSYNC_USERNAME")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            username_from_url
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("x-access-token")
                .to_string()
        });

    if allowed.contains(CredentialType::USERNAME) {
        return Cred::username(&username);
    }

    if allowed.contains(CredentialType::SSH_KEY) {
        if let Ok(key) = env::var("PANDORA_GITSYNC_SSH_KEY") {
            if !key.trim().is_empty() {
                let passphrase = env::var("PANDORA_GITSYNC_SSH_PASSPHRASE").ok();
                return Cred::ssh_key(
                    username_from_url.unwrap_or("git"),
                    None,
                    Path::new(&key),
                    passphrase.as_deref(),
                );
            }
        }
        if let Ok(cred) = Cred::ssh_key_from_agent(username_from_url.unwrap_or("git")) {
            return Ok(cred);
        }
    }

    if allowed.contains(CredentialType::USER_PASS_PLAINTEXT) {
        if let Ok(token) = env::var("PANDORA_GITSYNC_TOKEN") {
            if !token.trim().is_empty() {
                return Cred::userpass_plaintext(&username, &token);
            }
        }
        if let Some(config) = config {
            return Cred::credential_helper(config, url, username_from_url);
        }
    }

    Cred::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pandora-pull-{}-{}", name, std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Builds a linear history and returns each commit's oid, oldest first.
    fn repo_with_commits(dir: &Path, messages: &[&str]) -> (Repository, Vec<Oid>) {
        let repo = Repository::init(dir).unwrap();
        let signature = Signature::now("Pandora", "pandora@example.invalid").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let mut oids: Vec<Oid> = Vec::new();
        for message in messages {
            let tree = repo.find_tree(tree_id).unwrap();
            let parents = oids
                .last()
                .map(|oid| repo.find_commit(*oid).unwrap())
                .into_iter()
                .collect::<Vec<_>>();
            let oid = repo
                .commit(
                    Some("HEAD"),
                    &signature,
                    &signature,
                    message,
                    &tree,
                    &parents.iter().collect::<Vec<_>>(),
                )
                .unwrap();
            oids.push(oid);
        }
        (repo, oids)
    }

    #[test]
    fn head_commit_reports_the_short_id_and_the_title_line() {
        let dir = temp_dir("head");
        repo_with_commits(&dir, &["feat: add /refreshcache\n\nA body that is not the title."]);

        let commit = head_commit(dir.to_str().unwrap()).unwrap();
        assert_eq!(commit.title, "feat: add /refreshcache");
        assert!(commit.id.len() >= 7, "unexpected short id {}", commit.id);
        assert!(commit.id.chars().all(|c| c.is_ascii_hexdigit()), "{}", commit.id);
        assert_eq!(commit.label(), format!("@{} — feat: add /refreshcache", commit.id));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_directory_that_is_not_a_repository_reports_no_commit() {
        let dir = temp_dir("bare");
        assert!(head_commit(dir.to_str().unwrap()).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn only_the_commits_the_pull_introduced_are_listed_newest_first() {
        let dir = temp_dir("range");
        let (repo, oids) = repo_with_commits(&dir, &["one", "two", "three", "four"]);

        let (commits, truncated) = commits_between(&repo, oids[1], oids[3]);
        assert!(!truncated);
        assert_eq!(
            commits.iter().map(|c| c.title.as_str()).collect::<Vec<_>>(),
            vec!["four", "three"],
            "the already-present commits must not be relisted"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn an_unchanged_repository_reports_only_where_it_is() {
        let dir = temp_dir("uptodate");
        let (repo, oids) = repo_with_commits(&dir, &["only"]);

        let (commits, _) = commits_between(&repo, oids[0], oids[0]);
        assert!(commits.is_empty());

        let head = head_commit(dir.to_str().unwrap()).unwrap();
        let label = head.label();
        assert_eq!(SyncReport::at_head(head).lines(), vec![label]);

        std::fs::remove_dir_all(&dir).ok();
    }

    // A force-push leaves the previous tip unreachable, so nothing gets hidden and the walk would
    // otherwise cover the entire history.
    #[test]
    fn an_unreachable_previous_tip_caps_the_walk_instead_of_listing_everything() {
        let dir = temp_dir("unreachable");
        let messages = (0..MAX_SCANNED_COMMITS + 20)
            .map(|index| format!("commit {}", index))
            .collect::<Vec<_>>();
        let (repo, oids) = repo_with_commits(
            &dir,
            &messages.iter().map(String::as_str).collect::<Vec<_>>(),
        );

        let unreachable = Oid::from_str("0123456789012345678901234567890123456789").unwrap();
        let (commits, truncated) = commits_between(&repo, unreachable, *oids.last().unwrap());
        assert!(truncated);
        assert_eq!(commits.len(), MAX_SCANNED_COMMITS);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_long_backlog_lists_the_newest_commits_and_counts_the_rest() {
        let commits = (0..25)
            .map(|index| SyncedCommit {
                id: format!("{:07x}", index),
                title: format!("feat: change {}", index),
            })
            .collect::<Vec<_>>();
        let report = SyncReport {
            head: SyncedCommit {
                id: "0000000".to_string(),
                title: "feat: change 0".to_string(),
            },
            new_commits: commits,
            scan_truncated: false,
        };

        let lines = report.lines();
        assert_eq!(lines.len(), MAX_LISTED_COMMITS + 1);
        assert_eq!(lines[0], "@0000000 — feat: change 0");
        assert_eq!(lines.last().unwrap(), "…(+15)");
    }

    #[test]
    fn a_truncated_scan_marks_the_remainder_as_a_floor() {
        let report = SyncReport {
            head: SyncedCommit {
                id: "0000000".to_string(),
                title: "tip".to_string(),
            },
            new_commits: (0..MAX_LISTED_COMMITS)
                .map(|index| SyncedCommit {
                    id: format!("{:07x}", index),
                    title: format!("c{}", index),
                })
                .collect(),
            scan_truncated: true,
        };

        assert_eq!(report.lines().last().unwrap(), "…(+0+)");
    }
}
