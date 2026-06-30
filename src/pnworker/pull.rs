use git2::{
    AutotagOption, Cred, CredentialType, Error, FetchOptions, RemoteCallbacks, Repository,
};
use std::env;
use std::path::Path;

pub fn git_pull(repo_path: &str) -> Result<(), git2::Error> {
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

    Ok(())
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
