use git2::{
    AutotagOption, Cred, CredentialType, Error, FetchOptions, RemoteCallbacks, Repository,
};
use std::env;
use std::path::Path;

// The revision a sync landed on, so `/gitsync` and `/gitquery` can name the commit the restart will
// come up on instead of only saying that the pull worked. The title is the commit summary verbatim
// from git, so it is reported unlocalized like any other quoted source text.
pub struct SyncedCommit {
    pub id: String,
    pub title: String,
}

impl SyncedCommit {
    pub fn label(&self) -> String {
        format!("@{} — {}", self.id, self.title)
    }
}

// Reads HEAD without touching the network, so a failed pull can still report where the bot is.
pub fn head_commit(repo_path: &str) -> Option<SyncedCommit> {
    read_head_commit(&Repository::open(repo_path).ok()?)
}

fn read_head_commit(repo: &Repository) -> Option<SyncedCommit> {
    let commit = repo.head().ok()?.peel_to_commit().ok()?;
    // `short_id` honours the repo's own abbreviation length and grows it when a prefix collides.
    let id = commit
        .as_object()
        .short_id()
        .ok()
        .and_then(|buffer| buffer.as_str().map(str::to_string))
        .unwrap_or_else(|| commit.id().to_string().chars().take(7).collect());
    Some(SyncedCommit {
        id,
        title: commit.summary().unwrap_or("(no commit title)").to_string(),
    })
}

pub fn git_pull(repo_path: &str) -> Result<SyncedCommit, git2::Error> {
    let repo = Repository::open(repo_path)?;
    let config = repo.config().ok();
    let head = repo.head()?;
    let refname = head.name()
        .ok_or_else(|| git2::Error::from_str("invalid HEAD"))?
        .to_owned();
    let branch = head.shorthand()
        .ok_or_else(|| git2::Error::from_str("invalid branch"))?
        .to_owned();
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

    read_head_commit(&repo)
        .ok_or_else(|| git2::Error::from_str("HEAD does not point at a commit"))
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

    fn commit_repo(dir: &Path, message: &str) -> Repository {
        let repo = Repository::init(dir).unwrap();
        let signature = Signature::now("Pandora", "pandora@example.invalid").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &signature, &signature, message, &tree, &[])
            .unwrap();
        drop(tree);
        repo
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("pandora-pull-{}-{}", name, std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn head_commit_reports_the_short_id_and_the_title_line() {
        let dir = temp_dir("head");
        commit_repo(&dir, "feat: add /refreshcache\n\nA body that is not the title.");

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
}
