use git2::{Repository, FetchOptions, AutotagOption};

pub fn git_pull(repo_path: &str) -> Result<(), git2::Error> {
    let repo = Repository::open(repo_path)?;

    // 1. Fetch from origin
    let mut remote = repo.find_remote("origin")?;
    let mut fetch_opts = FetchOptions::new();
    fetch_opts.download_tags(AutotagOption::All);
    remote.fetch(&["refs/heads/*:refs/remotes/origin/*"], Some(&mut fetch_opts), None)?;

    // 2. Find the remote tracking branch (e.g. origin/main)
    let fetch_head = repo.find_reference("FETCH_HEAD")?;
    let fetch_commit = repo.reference_to_annotated_commit(&fetch_head)?;

    // 3. Fast-forward or merge
    let (analysis, _) = repo.merge_analysis(&[&fetch_commit])?;

    if analysis.is_fast_forward() {
        let refname = "refs/heads/main"; // adjust branch name
        let mut reference = repo.find_reference(refname)?;
        reference.set_target(fetch_commit.id(), "Fast-forward")?;
        repo.set_head(refname)?;
        repo.checkout_head(Some(git2::build::CheckoutBuilder::default().force()))?;
    } else if analysis.is_up_to_date() {
        println!("Already up to date.");
    } else {
        eprintln!("Merge required — fast-forward only supported here.");
    }

    Ok(())
}