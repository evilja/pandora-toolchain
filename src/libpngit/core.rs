use crate::libpnforgejo::core::{base64_encode, Forgejo};
use crate::libpnmal::core::{fetch_anime, AnimeKind, AnimeMeta};

pub struct Credits {
    pub tl: String,
    pub tlc: String,
    pub ts: String,
    pub qc: String,
}

impl Default for Credits {
    fn default() -> Self {
        Credits {
            tl: default_credit(),
            tlc: default_credit(),
            ts: default_credit(),
            qc: default_credit(),
        }
    }
}

pub struct RepoOutcome {
    pub label: &'static str,
    pub owner_repo: String,
    pub repo_url: String,
    pub name: String,
    pub slug: String,
    pub kind: String,
    pub episode_count: u32,
    pub season: u16,
    pub created: Vec<String>,
    pub renamed_files: Vec<String>,
}

pub struct SourceOutcome {
    pub path: String,
    pub content: String,
}

#[derive(serde::Serialize)]
pub struct Attachment {
    pub channel_id: String,
    pub mal_id: u64,
    pub name: String,
    pub slug: String,
    pub kind: String,
    pub episode_count: u32,
    pub season: u16,
    pub repo_url: String,
}

pub async fn list_attachments(server_id: u64) -> Vec<Attachment> {
    let dir = std::path::PathBuf::from("DB").join("config").join(server_id.to_string());
    let mut rd = match tokio::fs::read_dir(&dir).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<Attachment> = Vec::new();
    while let Ok(Some(entry)) = rd.next_entry().await {
        let channel_id = match entry.file_name().to_str().and_then(|s| s.parse::<u64>().ok()) {
            Some(id) => id,
            None => continue,
        };
        let meta = read_channel_meta(server_id, channel_id);
        if meta.mal_id.is_none() || meta.kind.is_none() {
            continue;
        }
        out.push(Attachment {
            channel_id: channel_id.to_string(),
            mal_id: meta.mal_id.unwrap_or(0),
            name: meta.name.unwrap_or_default(),
            slug: meta.slug.unwrap_or_default(),
            kind: meta.kind.unwrap_or_default(),
            episode_count: meta.episode_count.unwrap_or(0),
            season: meta.season,
            repo_url: meta.repo_url.unwrap_or_default(),
        });
    }
    out.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    out
}

pub async fn init_repo(
    server_id: u64,
    channel_id: u64,
    mal_url: &str,
    season: u16,
    credits: &Credits,
) -> Result<RepoOutcome, String> {
    attach_or_init(server_id, channel_id, mal_url, None, season, credits, true).await
}

pub async fn attach_repo(
    server_id: u64,
    channel_id: u64,
    mal_url: &str,
    repo_url: &str,
    season: u16,
    credits: &Credits,
) -> Result<RepoOutcome, String> {
    attach_or_init(server_id, channel_id, mal_url, Some(repo_url.to_string()), season, credits, false).await
}

async fn attach_or_init(
    server_id: u64,
    channel_id: u64,
    mal_url: &str,
    repo_arg: Option<String>,
    season: u16,
    credits: &Credits,
    is_init: bool,
) -> Result<RepoOutcome, String> {
    let label = if is_init { "/init" } else { "/attach" };

    if !is_init && repo_arg.as_deref().map(str::trim).unwrap_or("").is_empty() {
        return Err("`repo` is required for /attach".to_string());
    }

    let existing = read_channel_meta(server_id, channel_id);
    let (forgejo_base, api_key) = forgejo_config(server_id).await?;

    let meta = fetch_anime(mal_url).await.map_err(|e| format!("MAL fetch failed: {}", e))?;

    if let Some(eid) = existing.mal_id {
        if eid != meta.mal_id {
            return Err(format!(
                "Channel is already attached to `{}`. Use a different channel to attach a new anime.",
                existing.name.unwrap_or_default()
            ));
        }
    }

    let fg = Forgejo::new(forgejo_base, api_key).map_err(|e| format!("Forgejo init failed: {}", e))?;

    let (owner_repo, repo_url) = if is_init {
        let repo_slug = meta.slug.replace('-', "_");
        let or = format!("{}/{}", fg.org, repo_slug);
        let url = fg.create_repo(&repo_slug).await.map_err(|e| format!("create_repo failed: {}", e))?;
        (or, url)
    } else {
        let repo_url = repo_arg.unwrap();
        let (owner, repo) = parse_repo_url(&repo_url).map_err(|e| format!("Bad repo URL: {}", e))?;
        (format!("{}/{}", owner, repo), repo_url)
    };

    let existing_root = fg.list_contents(&owner_repo, "").await
        .map_err(|e| format!("list_contents failed: {}", e))?;

    let episode_count_at_git = count_existing_episodes(&existing_root, meta.episode_count);

    let base_md = tokio::fs::read_to_string(format!("DB/config/{}/base.md", server_id))
        .await
        .ok()
        .map(|t| substitute_base_md(&t, &meta, &repo_url, episode_count_at_git, season, credits));

    let created = bootstrap_repo(&fg, &owner_repo, &meta, base_md, existing_root).await
        .map_err(|e| format!("Bootstrap failed: {}", e))?;

    let mut renamed_files: Vec<String> = Vec::new();
    if !is_init {
        let safe_name = meta.name.replace('/', "-");
        for n in 1..=meta.episode_count {
            let folder = pad2(n);
            let entries = match fg.list_contents(&owner_repo, &folder).await {
                Ok(v) => v,
                Err(_) => continue,
            };
            let ass_files: Vec<String> = entries.into_iter()
                .filter(|name| name.to_lowercase().ends_with(".ass"))
                .collect();
            if ass_files.len() != 1 {
                continue;
            }
            let old_name = ass_files.into_iter().next().unwrap();
            let old_path = format!("{}/{}", folder, old_name);
            let new_name = format!("TL - {} - E{:02}.ass", safe_name, n);
            let new_path = format!("{}/{}", folder, new_name);
            if old_path == new_path {
                continue;
            }
            fg.move_file(&owner_repo, &old_path, &new_path, "attach: rename to standard filename").await
                .map_err(|e| format!("move_file failed ({}): {}", old_path, e))?;
            renamed_files.push(format!("{} -> {}", folder, new_name));
        }
    }

    let new_meta = ChannelMeta {
        mal_id: Some(meta.mal_id),
        kind: Some(kind_label(&meta.kind).to_string()),
        name: Some(meta.name.clone()),
        slug: Some(meta.slug.clone()),
        episode_count: Some(meta.episode_count),
        repo_url: Some(repo_url.clone()),
        episode_count_at_git: Some(episode_count_at_git),
        year: meta.year,
        season,
        tl: credits.tl.clone(),
        tlc: credits.tlc.clone(),
        ts: credits.ts.clone(),
        qc: credits.qc.clone(),
        acix_template: existing.acix_template,
    };
    write_channel_meta(server_id, channel_id, &new_meta).await
        .map_err(|e| format!("Failed to save channel meta: {}", e))?;

    Ok(RepoOutcome {
        label,
        owner_repo,
        repo_url,
        name: meta.name.clone(),
        slug: meta.slug.clone(),
        kind: kind_label(&meta.kind).to_string(),
        episode_count: meta.episode_count,
        season,
        created,
        renamed_files,
    })
}

pub async fn set_source(
    server_id: u64,
    channel_id: u64,
    episode: u32,
    link: &str,
) -> Result<SourceOutcome, String> {
    let meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() {
        return Err("this channel is not attached to an anime. Run /init or /attach first.".to_string());
    }
    let max_ep = meta.episode_count.unwrap_or(0);
    if episode < 1 || episode > max_ep {
        return Err(format!("`episode` must be between 1 and {}.", max_ep));
    }
    let repo_url = meta.repo_url.clone().filter(|s| !s.is_empty())
        .ok_or_else(|| "this channel has no repo URL configured.".to_string())?;
    let (owner, repo) = parse_repo_url(&repo_url).map_err(|e| format!("bad repo URL in meta: {}", e))?;
    let owner_repo = format!("{}/{}", owner, repo);

    let (forgejo_base, api_key) = forgejo_config(server_id).await?;
    let fg = Forgejo::new(forgejo_base, api_key).map_err(|e| format!("Forgejo init failed: {}", e))?;

    let folder = pad2(episode);
    let source_path = format!("{}/SOURCE.md", folder);
    let source_content = format!("# {}\n", source_link(link));
    let source_b64 = base64_encode(&source_content);
    fg.upsert_file(&owner_repo, &source_path, &source_b64, "Set source link").await
        .map_err(|e| format!("Failed to write {}: {}", source_path, e))?;
    remove_gitkeep_for_path(&fg, &owner_repo, &source_path).await;

    Ok(SourceOutcome { path: source_path, content: source_content })
}

async fn forgejo_config(server_id: u64) -> Result<(String, String), String> {
    let (_lang, forgejo_base, api_key) = read_server_meta(server_id).await
        .map_err(|e| format!("failed to read server meta: {}", e))?;
    if forgejo_base.is_empty() {
        return Err("server has no forgejo org configured. Run /configure first.".to_string());
    }
    Ok((forgejo_base, api_key))
}

async fn read_server_meta(server_id: u64) -> Result<(String, String, String), String> {
    let path = format!("DB/config/{}/meta.pandora", server_id);
    let s = tokio::fs::read_to_string(&path).await.map_err(|e| e.to_string())?;
    let mut lines = s.lines();
    let lang = lines.next().unwrap_or("tr").to_string();
    let forgejo = lines.next().unwrap_or("").to_string();
    let _channel_id = lines.next().unwrap_or("").to_string();
    let api_key = lines.next().unwrap_or("").to_string();
    Ok((lang, forgejo, api_key))
}

#[derive(serde::Deserialize, Default)]
struct ChannelMeta {
    mal_id: Option<u64>,
    kind: Option<String>,
    name: Option<String>,
    slug: Option<String>,
    episode_count: Option<u32>,
    repo_url: Option<String>,
    episode_count_at_git: Option<u32>,
    year: Option<u16>,
    #[serde(default = "default_season")]
    season: u16,
    #[serde(default = "default_credit")]
    tl: String,
    #[serde(default = "default_credit")]
    tlc: String,
    #[serde(default = "default_credit")]
    ts: String,
    #[serde(default = "default_credit")]
    qc: String,
    #[serde(default)]
    acix_template: Option<i64>,
}

fn default_season() -> u16 { 1 }

fn default_credit() -> String { "---".to_string() }

fn meta_path(server_id: u64, channel_id: u64) -> std::path::PathBuf {
    std::path::PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join(channel_id.to_string())
        .join("meta.toml")
}

fn read_channel_meta(server_id: u64, channel_id: u64) -> ChannelMeta {
    let path = meta_path(server_id, channel_id);
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).unwrap_or_default(),
        Err(_) => ChannelMeta::default(),
    }
}

async fn write_channel_meta(server_id: u64, channel_id: u64, m: &ChannelMeta) -> Result<(), String> {
    let path = meta_path(server_id, channel_id);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|e| e.to_string())?;
    }
    tokio::fs::write(&path, meta_to_toml(m)).await.map_err(|e| e.to_string())?;
    Ok(())
}

fn meta_to_toml(m: &ChannelMeta) -> String {
    match (&m.kind, m.mal_id) {
        (Some(k), Some(id)) => {
            let mut out = format!(
                "mal_id = {}\nkind = \"{}\"\nname = \"{}\"\nslug = \"{}\"\nepisode_count = {}\nrepo_url = \"{}\"\nseason = {}\ntl = \"{}\"\ntlc = \"{}\"\nts = \"{}\"\nqc = \"{}\"\n",
                id, k, m.name.as_deref().unwrap_or(""), m.slug.as_deref().unwrap_or(""),
                m.episode_count.unwrap_or(0), m.repo_url.as_deref().unwrap_or(""),
                m.season, m.tl, m.tlc, m.ts, m.qc
            );
            if let Some(y) = m.year {
                out.push_str(&format!("year = {}\n", y));
            }
            if let Some(c) = m.episode_count_at_git {
                out.push_str(&format!("episode_count_at_git = {}\n", c));
            }
            if let Some(t) = m.acix_template {
                out.push_str(&format!("acix_template = {}\n", t));
            }
            out
        }
        _ => String::new(),
    }
}

fn pad2(n: u32) -> String {
    if n < 100 {
        format!("{:02}", n)
    } else {
        n.to_string()
    }
}

fn parse_repo_url(url: &str) -> Result<(String, String), String> {
    let re = regex::Regex::new(r"^https?://[^/]+/([^/]+)/([^/]+)/?$").unwrap();
    let caps = re.captures(url.trim_end_matches('/'))
        .ok_or_else(|| format!("not a Forgejo repo URL: {}", url))?;
    let owner = caps.get(1).unwrap().as_str().to_string();
    let repo = caps.get(2).unwrap().as_str().to_string();
    Ok((owner, repo))
}

fn kind_label(k: &AnimeKind) -> &'static str {
    match k {
        AnimeKind::Movie => "Movie",
        AnimeKind::MultiEpisode => "MultiEpisode",
    }
}

fn count_existing_episodes(existing: &[String], max: u32) -> u32 {
    existing.iter()
        .filter_map(|n| n.trim_start_matches('0').parse::<u32>().ok().filter(|&v| v >= 1))
        .filter(|&n| n <= max)
        .count() as u32
}

fn substitute_base_md(
    template: &str,
    meta: &AnimeMeta,
    repo_url: &str,
    episode_count_at_git: u32,
    season: u16,
    credits: &Credits,
) -> String {
    let mut out = template.to_string();
    let pairs: Vec<(&str, String)> = vec![
        ("name", meta.name.clone()),
        ("slug", meta.slug.clone()),
        ("kind", kind_label(&meta.kind).to_string()),
        ("mal_id", meta.mal_id.to_string()),
        ("episode_count", meta.episode_count.to_string()),
        ("year", meta.year.map(|y| y.to_string()).unwrap_or_default()),
        ("repo_url", repo_url.to_string()),
        ("episode_count_at_git", episode_count_at_git.to_string()),
        ("season", season.to_string()),
        ("tl", credits.tl.clone()),
        ("tlc", credits.tlc.clone()),
        ("ts", credits.ts.clone()),
        ("qc", credits.qc.clone()),
    ];
    for (key, val) in &pairs {
        out = out.replace(&format!("%{}%", key), val);
    }
    out
}

async fn bootstrap_repo(
    fg: &Forgejo,
    owner_repo: &str,
    meta: &AnimeMeta,
    base_md: Option<String>,
    existing: Vec<String>,
) -> Result<Vec<String>, String> {
    let mut created: Vec<String> = Vec::new();

    let existing_nums: Vec<u32> = existing.iter()
        .filter_map(|n| n.trim_start_matches('0').parse::<u32>().ok().filter(|v| *v > 0).or_else(|| {
            if n == "0" { Some(0) } else { None }
        }))
        .collect();

    let empty_b64 = base64_encode("");

    for n in 1..=meta.episode_count {
        if existing_nums.contains(&n) { continue; }
        let folder = pad2(n);
        let path = format!("{}/.gitkeep", folder);
        fg.create_file(owner_repo, &path, &empty_b64, "bootstrap episode folder").await?;
        created.push(folder);
    }

    let has_readme = existing.iter().any(|n| n.eq_ignore_ascii_case("README.md"));
    let has_gitignore = existing.iter().any(|n| n.eq_ignore_ascii_case(".gitignore"));
    if !has_gitignore {
        fg.create_file(owner_repo, ".gitignore", &base64_encode("*.mkv\n"), "bootstrap gitignore").await?;
        created.push(".gitignore".to_string());
    }
    if let Some(readme) = base_md {
        let b64 = base64_encode(&readme);
        if has_readme {
            let sha = fg.get_file_sha(owner_repo, "README.md").await?
                .ok_or_else(|| "README.md disappeared between list and update".to_string())?;
            fg.update_file(owner_repo, "README.md", &b64, &sha, "bootstrap root readme").await?;
        } else {
            fg.create_file(owner_repo, "README.md", &b64, "bootstrap root readme").await?;
        }
        created.push("README.md".to_string());
    }

    Ok(created)
}

fn source_link(link: &str) -> String {
    let trimmed = link.trim();
    let re = regex::Regex::new(r"^(https://nyaa\.(?:si|land))/(?:download|view)/([0-9]+)(?:\.torrent|/torrent)?/?$").unwrap();
    match re.captures(trimmed) {
        Some(caps) => format!("{}/view/{}", caps.get(1).unwrap().as_str(), caps.get(2).unwrap().as_str()),
        None => trimmed.to_string(),
    }
}

async fn remove_gitkeep_for_path(fg: &Forgejo, owner_repo: &str, path: &str) {
    let folder = match path.rsplit_once('/') {
        Some((folder, _)) if !folder.is_empty() => folder,
        _ => return,
    };
    let gitkeep = format!("{}/.gitkeep", folder);
    if let Ok(Some(sha)) = fg.get_file_sha(owner_repo, &gitkeep).await {
        let _ = fg.delete_file(owner_repo, &gitkeep, &sha, "Remove .gitkeep").await;
    }
}
