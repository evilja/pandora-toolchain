use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::libkagami::core::SubstationAlpha;
use crate::libpnenv::core::get_pandora_env;
use crate::libpnenv::standard::PNASS;
use crate::libpnforgejo::core::{base64_encode, base64_encode_bytes, Forgejo};
use crate::libpnmal::core::{fetch_anime, AnimeKind, AnimeMeta};
use crate::libpnprotocol::core::Protocol;
use crate::pnworker::tools::{PNASS_MERGE, PNASS_MERGE_TL_ONLY, PNASS_SPLIT_SIGNS};
use crate::pnworker::util::{run_tool, CliParam, PathValue, ToolResult};

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

pub struct DetachOutcome {
    pub name: String,
    pub repo_url: String,
}

pub async fn detach_channel(server_id: u64, channel_id: u64) -> Result<DetachOutcome, String> {
    let meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() && meta.repo_url.as_deref().map_or(true, str::is_empty) {
        return Err("this channel is not attached to an anime.".to_string());
    }
    let out = DetachOutcome {
        name: meta.name.unwrap_or_default(),
        repo_url: meta.repo_url.unwrap_or_default(),
    };
    let path = meta_path(server_id, channel_id);
    tokio::fs::remove_file(&path).await.map_err(|e| format!("failed to remove channel meta: {}", e))?;
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::remove_dir(parent).await;
    }
    Ok(out)
}

pub struct DestructOutcome {
    pub name: String,
    pub owner_repo: String,
}

pub async fn destruct_repo(server_id: u64, channel_id: u64) -> Result<DestructOutcome, String> {
    let meta = read_channel_meta(server_id, channel_id);
    if meta.mal_id.is_none() {
        return Err("this channel is not attached to an anime.".to_string());
    }
    let repo_url = meta.repo_url.clone().filter(|s| !s.is_empty())
        .ok_or_else(|| "this channel has no repo URL configured.".to_string())?;
    let (owner, repo) = parse_repo_url(&repo_url).map_err(|e| format!("bad repo URL in meta: {}", e))?;
    let owner_repo = format!("{}/{}", owner, repo);
    let name = meta.name.clone().unwrap_or_default();

    let (forgejo_base, api_key) = forgejo_config(server_id).await?;
    let fg = Forgejo::new(forgejo_base, api_key).map_err(|e| format!("Forgejo init failed: {}", e))?;
    fg.delete_repo(&owner_repo).await.map_err(|e| format!("delete_repo failed: {}", e))?;

    let _ = tokio::fs::remove_file(meta_path(server_id, channel_id)).await;
    Ok(DestructOutcome { name, owner_repo })
}

pub struct SmartMergeResult {
    pub link: String,
    pub merged_bytes: Vec<u8>,
    pub owner_repo: String,
    pub release_path: String,
    pub source_path: String,
    pub gdrive_folder_global: String,
    pub gdrive_folder_local: String,
    pub warnings: Vec<String>,
}

pub async fn smartcode_merge(
    server_id: u64,
    channel_id: u64,
    episode: u32,
    link_opt: Option<String>,
) -> Result<SmartMergeResult, String> {
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
    let name = meta.name.clone().unwrap_or_default();

    let (forgejo_base, api_key) = forgejo_config(server_id).await?;
    let fg = Forgejo::new(forgejo_base, api_key).map_err(|e| format!("Forgejo init failed: {}", e))?;

    let safe_name = name.replace('/', "-");
    let folder = pad2(episode);
    let tl_path = format!("{}/TL - {} - E{:02}.ass", folder, safe_name, episode);
    let ts_path = format!("{}/TS - {} - E{:02}.ass", folder, safe_name, episode);

    let tl_bytes = match read_repo_ass(&fg, &owner_repo, &tl_path).await? {
        Some((b, _)) => b,
        None => return Err(format!("TL file not found at {} or {}.zip.", tl_path, tl_path)),
    };
    let mut ts_bytes_opt = read_repo_ass(&fg, &owner_repo, &ts_path).await?.map(|(b, _)| b);

    let link = match link_opt {
        Some(ref l) => l.clone(),
        None => {
            let source_md_path = format!("{}/SOURCE.md", folder);
            let b64 = fg.get_file_content(&owner_repo, &source_md_path).await?
                .ok_or_else(|| format!("`link` was not provided and no {} exists in the repo to read it from.", source_md_path))?
                .0;
            let bytes = base64_decode_bytes(&b64).map_err(|e| format!("failed to decode {}: {}", source_md_path, e))?;
            let text = String::from_utf8(bytes).map_err(|e| format!("{} is not valid UTF-8: {}", source_md_path, e))?;
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with(';'))
                .map(|l| l.trim_start_matches('#').trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("{} does not contain a parseable source link.", source_md_path))?
        }
    };

    let pnass_path = match get_pandora_env().get(PNASS) {
        Some(p) if !p.is_empty() => p.clone(),
        _ => return Err("PNASS binary path is not set in DB/config/global/environment/env.pandora.".to_string()),
    };

    let job_id = nano_id();
    let wrap_style = server_wrap_style(server_id);
    let work_dir = std::env::temp_dir().join(format!("pandora_smartcode_{}", job_id));
    tokio::fs::create_dir_all(&work_dir).await.map_err(|e| format!("failed to create work dir: {}", e))?;

    let result = smartcode_merge_inner(
        &fg, &owner_repo, &tl_path, &ts_path, &folder, &safe_name, episode,
        tl_bytes, &mut ts_bytes_opt, &link, link_opt.is_some(), &pnass_path, &wrap_style, job_id, &work_dir,
    ).await;
    let _ = tokio::fs::remove_dir_all(&work_dir).await;

    let (merged_bytes, release_path, source_path, warnings) = result?;
    Ok(SmartMergeResult {
        link,
        merged_bytes,
        gdrive_folder_global: smartcode_global_drive_folder(&owner_repo, &safe_name),
        gdrive_folder_local: smartcode_local_drive_folder(&safe_name),
        owner_repo,
        release_path,
        source_path,
        warnings,
    })
}

fn smartcode_global_drive_folder(owner_repo: &str, safe_name: &str) -> String {
    let owner = owner_repo.split('/').next().unwrap_or("").trim();
    if owner.is_empty() {
        smartcode_local_drive_folder(safe_name)
    } else {
        format!("{}/{}", drive_folder_component(owner), drive_folder_component(safe_name))
    }
}

fn smartcode_local_drive_folder(safe_name: &str) -> String {
    drive_folder_component(safe_name)
}

fn drive_folder_component(s: &str) -> String {
    s.replace('/', "-").trim().to_string()
}

#[allow(clippy::too_many_arguments)]
async fn smartcode_merge_inner(
    fg: &Forgejo,
    owner_repo: &str,
    tl_path: &str,
    ts_path: &str,
    folder: &str,
    safe_name: &str,
    episode: u32,
    tl_bytes: Vec<u8>,
    ts_bytes_opt: &mut Option<Vec<u8>>,
    link: &str,
    link_from_arg: bool,
    pnass_path: &str,
    wrap_style: &str,
    job_id: u64,
    work_dir: &Path,
) -> Result<(Vec<u8>, String, String, Vec<String>), String> {
    let tl_local = work_dir.join("tl.ass");
    let ts_local = work_dir.join("ts.ass");
    let merged_local = work_dir.join("merged.ass");
    let split_tl_local = work_dir.join("tl_no_signs.ass");
    let mut warnings: Vec<String> = Vec::new();

    tokio::fs::write(&tl_local, &tl_bytes).await.map_err(|e| format!("failed to write TL: {}", e))?;
    if let Some(b) = ts_bytes_opt.as_deref() {
        tokio::fs::write(&ts_local, b).await.map_err(|e| format!("failed to write TS: {}", e))?;
    } else {
        let mut proto = Protocol::new(vec![1]);
        let split_result = run_tool(
            pnass_path,
            PNASS_SPLIT_SIGNS,
            &HashMap::from([
                ("INPUT",  PathValue::from(tl_local.display().to_string())),
                ("OUTPUT", PathValue::from(split_tl_local.display().to_string())),
                ("SIGNS",  PathValue::from(ts_local.display().to_string())),
                ("WRAPSTYLE", PathValue::from(wrap_style.to_string())),
            ]),
            job_id,
            &mut proto,
            |data| {
                if data.get(0).and_then(|v| v.as_str()) == Some("4") {
                    if let Some(line) = data.get(1).and_then(|v| v.as_str()) {
                        warnings.push(line.to_string());
                    }
                }
                None
            },
        ).await;
        if !matches!(split_result, ToolResult::Success) {
            return Err(format!("ASS sign split failed (warnings so far: {}).", warnings.len()));
        }
        if tokio::fs::metadata(&ts_local).await.is_ok() {
            let split_tl_bytes = tokio::fs::read(&split_tl_local).await
                .map_err(|e| format!("failed to read sign-aware TL: {}", e))?;
            let sign_bytes = tokio::fs::read(&ts_local).await
                .map_err(|e| format!("failed to read generated TS: {}", e))?;
            upsert_repo_ass(fg, owner_repo, tl_path, &split_tl_bytes, "Smartcode move signs from TL").await?;
            upsert_repo_ass(fg, owner_repo, ts_path, &sign_bytes, "Smartcode move signs to TS").await?;
            tokio::fs::write(&tl_local, &split_tl_bytes).await
                .map_err(|e| format!("failed to write sign-aware TL: {}", e))?;
            warnings.push(format!("Sign lines were moved from {} into generated {}.", tl_path, ts_path));
            *ts_bytes_opt = Some(sign_bytes);
        }
    }

    let spec: &[CliParam] = if ts_bytes_opt.is_some() { PNASS_MERGE } else { PNASS_MERGE_TL_ONLY };
    let mut paths: HashMap<&str, PathValue> = HashMap::from([
        ("INPUT",  PathValue::from(tl_local.display().to_string())),
        ("OUTPUT", PathValue::from(merged_local.display().to_string())),
        ("WRAPSTYLE", PathValue::from(wrap_style.to_string())),
    ]);
    if ts_bytes_opt.is_some() {
        paths.insert("MERGE", PathValue::from(ts_local.display().to_string()));
    }

    let mut proto = Protocol::new(vec![1]);
    let result = run_tool(
        pnass_path,
        spec,
        &paths,
        job_id,
        &mut proto,
        |data| {
            if data.get(0).and_then(|v| v.as_str()) == Some("4") {
                if let Some(line) = data.get(1).and_then(|v| v.as_str()) {
                    warnings.push(line.to_string());
                }
            }
            None
        },
    ).await;
    if !matches!(result, ToolResult::Success) {
        return Err(format!("ASS merge failed (warnings so far: {}).", warnings.len()));
    }

    let merged_sub = SubstationAlpha::load(merged_local.clone(), true).await;
    if merged_sub.dump_to_file(merged_local.clone()).await.is_err() {
        return Err("failed to write advanced-parsed merged ASS.".to_string());
    }

    let merged_bytes = tokio::fs::read(&merged_local).await
        .map_err(|e| format!("failed to read merged ASS: {}", e))?;

    let release_path = format!("{}/Release - {} - E{:02}.ass", folder, safe_name, episode);
    let uploaded_release_path = upsert_repo_ass(fg, owner_repo, &release_path, &merged_bytes, "Smartcode merge").await
        .map_err(|e| format!("merged ASS upload to {} failed: {}", release_path, e))?;

    let source_path = format!("{}/SOURCE.md", folder);
    if link_from_arg {
        let source_content = format!("# {}\n", source_link(link));
        let source_b64 = base64_encode(&source_content);
        fg.upsert_file(owner_repo, &source_path, &source_b64, "Smartcode source").await
            .map_err(|e| format!("SOURCE.md upload to {} failed: {}", source_path, e))?;
        remove_gitkeep_for_path(fg, owner_repo, &source_path).await;
    }

    Ok((merged_bytes, uploaded_release_path, source_path, warnings))
}

fn nano_id() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
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

fn server_wrap_style(server_id: u64) -> String {
    let path = format!("DB/config/{}/meta.pandora", server_id);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| s.lines().nth(8).map(String::from))
        .filter(|s| matches!(s.as_str(), "0" | "1" | "2" | "3"))
        .unwrap_or_else(|| "keep".to_string())
}

const ASS_ZIP_THRESHOLD_BYTES: usize = 1_500_000;

async fn read_repo_ass(fg: &Forgejo, owner_repo: &str, ass_path: &str) -> Result<Option<(Vec<u8>, String)>, String> {
    let zip_path = format!("{}.zip", ass_path);
    if let Some((b64, _)) = fg.get_file_content(owner_repo, &zip_path).await? {
        let zip_bytes = base64_decode_bytes(&b64)?;
        let ass_bytes = unzip_single_ass(&zip_bytes).await?;
        return Ok(Some((ass_bytes, zip_path)));
    }
    match fg.get_file_content(owner_repo, ass_path).await? {
        Some((b64, _)) => Ok(Some((base64_decode_bytes(&b64)?, ass_path.to_string()))),
        None => Ok(None),
    }
}

async fn upsert_repo_ass(fg: &Forgejo, owner_repo: &str, ass_path: &str, bytes: &[u8], message: &str) -> Result<String, String> {
    let (upload_path, upload_bytes, alternate_path) = if bytes.len() > ASS_ZIP_THRESHOLD_BYTES {
        let entry_name = Path::new(ass_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("subtitle.ass");
        (format!("{}.zip", ass_path), zip_single_ass(entry_name, bytes).await?, ass_path.to_string())
    } else {
        (ass_path.to_string(), bytes.to_vec(), format!("{}.zip", ass_path))
    };
    let b64 = base64_encode_bytes(&upload_bytes);
    fg.upsert_file(owner_repo, &upload_path, &b64, message).await?;
    remove_gitkeep_for_path(fg, owner_repo, &upload_path).await;
    if let Ok(Some(sha)) = fg.get_file_sha(owner_repo, &alternate_path).await {
        let _ = fg.delete_file(owner_repo, &alternate_path, &sha, &format!("Remove alternate {}", alternate_path)).await;
    }
    Ok(upload_path)
}

async fn zip_single_ass(entry_name: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::new();
    {
        let mut writer = async_zip::base::write::ZipFileWriter::new(&mut out);
        let entry = async_zip::ZipEntryBuilder::new(entry_name.to_string().into(), async_zip::Compression::Deflate);
        writer.write_entry_whole(entry, bytes).await.map_err(|e| e.to_string())?;
        writer.close().await.map_err(|e| e.to_string())?;
    }
    Ok(out)
}

async fn unzip_single_ass(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let dir = std::env::temp_dir().join(format!("pandora_repo_ass_zip_{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()));
    let result = async {
        tokio::fs::create_dir_all(&dir).await.map_err(|e| e.to_string())?;
        let extracted = extract_zip_root_ass(bytes, &dir).await?
            .ok_or_else(|| "zip must contain exactly one root .ass file".to_string())?;
        tokio::fs::read(extracted).await.map_err(|e| e.to_string())
    }.await;
    let _ = tokio::fs::remove_dir_all(&dir).await;
    result
}

async fn extract_zip_root_ass(bytes: &[u8], dest: &Path) -> Result<Option<PathBuf>, String> {
    use async_zip::base::read::stream::ZipFileReader;
    use futures_lite::io::AsyncReadExt;
    use tokio::io::{AsyncWriteExt, BufReader};

    let tmp = std::env::temp_dir().join(format!("pandora_repo_zip_{}.zip",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos()));

    let result = async {
        {
            let mut f = tokio::fs::File::create(&tmp).await.map_err(|e| e.to_string())?;
            f.write_all(bytes).await.map_err(|e| e.to_string())?;
            f.sync_all().await.map_err(|e| e.to_string())?;
        }
        let f = tokio::fs::File::open(&tmp).await.map_err(|e| e.to_string())?;
        let mut zip = ZipFileReader::with_tokio(BufReader::new(f));

        let mut found: Option<PathBuf> = None;
        let mut count: usize = 0;

        loop {
            let mut entry = match zip.next_with_entry().await.map_err(|e| format!("zip: {}", e))? {
                Some(e) => e,
                None => break,
            };
            let filename = entry.reader().entry().filename().as_str()
                .map_err(|e| format!("zip filename: {}", e))?
                .to_string();
            let is_ass = filename.to_lowercase().ends_with(".ass");

            if is_ass {
                if filename.contains('\\') {
                    return Err(format!("zip contains unsafe .ass path: {}", filename));
                }
                let mut components = Path::new(&filename).components();
                let safe_name = match (components.next(), components.next()) {
                    (Some(std::path::Component::Normal(name)), None) => name.to_owned(),
                    _ => return Err(format!("zip contains unsafe .ass path: {}", filename)),
                };
                count += 1;
                if count > 1 {
                    return Ok(None);
                }
                let mut data = Vec::new();
                entry.reader_mut().read_to_end(&mut data).await
                    .map_err(|e| format!("zip read: {}", e))?;
                let out_path = dest.join(safe_name);
                tokio::fs::write(&out_path, &data).await.map_err(|e| e.to_string())?;
                found = Some(out_path);
            }

            zip = entry.skip().await.map_err(|e| format!("zip skip: {}", e))?;
        }

        Ok(found)
    }.await;

    let _ = tokio::fs::remove_file(&tmp).await;
    result
}

fn base64_decode_bytes(input: &str) -> Result<Vec<u8>, String> {
    const ALPH: [u8; 128] = {
        let mut a = [255u8; 128];
        let chars = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut i = 0;
        while i < chars.len() {
            a[chars[i] as usize] = i as u8;
            i += 1;
        }
        a
    };
    let cleaned: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if cleaned.len() % 4 != 0 {
        return Err(format!("base64: invalid length {}", cleaned.len()));
    }
    let mut out: Vec<u8> = Vec::with_capacity(cleaned.len() / 4 * 3);
    let mut i = 0;
    while i < cleaned.len() {
        let c0 = cleaned[i];
        let c1 = cleaned[i + 1];
        let c2 = cleaned[i + 2];
        let c3 = cleaned[i + 3];
        let pad2 = c2 == b'=';
        let pad3 = c3 == b'=';
        let idx = |b: u8| -> u8 { if b < 128 { ALPH[b as usize] } else { 255 } };
        let v0 = idx(c0);
        let v1 = idx(c1);
        if v0 == 255 || v1 == 255 {
            return Err(format!("base64: invalid char at {}", i));
        }
        if !pad2 {
            let v2 = idx(c2);
            if v2 == 255 {
                return Err(format!("base64: invalid char at {}", i + 2));
            }
            out.push((v0 << 2) | (v1 >> 4));
            if !pad3 {
                let v3 = idx(c3);
                if v3 == 255 {
                    return Err(format!("base64: invalid char at {}", i + 3));
                }
                out.push((v1 << 4) | (v2 >> 2));
                out.push((v2 << 6) | v3);
            } else {
                out.push((v1 << 4) | (v2 >> 2));
            }
        } else {
            out.push((v0 << 2) | (v1 >> 4));
        }
        i += 4;
    }
    Ok(out)
}
