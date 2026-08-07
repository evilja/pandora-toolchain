use super::*;
use pandora_toolchain::lib::env::standard::{
    ABYSS, CLIENT_ID, CLIENT_SECRET, DOODSTREAM, ENV_PATH, ENV_SEP, LULU, PARENTID, REFRESH_TOKEN,
    TOKEN_URL, UPLOAD_URL, UQLOAD, VOESX,
};
use serde::{Deserialize, Serialize};
use serenity::builder::CreateAttachment;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

const KEYVAULT_VERSION: u8 = 1;
const KEYVAULT_TTL_SECS: u64 = 60 * 60;
const MAX_BACKUP_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BACKUP_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENCRYPTED_BACKUP_BYTES: usize = 24 * 1024 * 1024;
const MAX_SCANNED_ENTRIES: usize = 100_000;
const KEYVAULT_DIR: &str = "DB/config/global/environment/keyvault";
const PENDING_PATH: &str = "DB/config/global/environment/keyvault/pending.json";
const LEGACY_ENV_KEYS: &[&str] = &[
    CLIENT_ID,
    CLIENT_SECRET,
    REFRESH_TOKEN,
    TOKEN_URL,
    UPLOAD_URL,
    PARENTID,
    DOODSTREAM,
    UQLOAD,
    LULU,
    VOESX,
    ABYSS,
];
const LEGACY_ENV_LINE_INDICES: &[usize] = &[0, 1, 2, 3, 5, 9, 10, 11, 12, 13, 14];
const LEGACY_GUILD_LINE_INDICES: &[usize] = &[4, 5, 6, 7, 10];

struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

impl Drop for SensitiveBytes {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

struct BackupEntry {
    source_key: Option<String>,
    bytes: SensitiveBytes,
}

struct BackupCollection {
    entries: BTreeMap<String, BackupEntry>,
    process_environment_names: Vec<String>,
    notes: Vec<String>,
}

struct PlannedWrite {
    path: PathBuf,
    original: SensitiveBytes,
    replacement: SensitiveBytes,
    values_removed: usize,
    kind: PurgeWriteKind,
}

#[derive(Clone, Copy)]
enum PurgeWriteKind {
    Environment,
    Guild,
}

struct PlannedDelete {
    path: PathBuf,
    original: SensitiveBytes,
}

struct PurgePlan {
    writes: Vec<PlannedWrite>,
    deletes: Vec<PlannedDelete>,
    fingerprint: String,
}

#[derive(Clone, Copy, Default, Serialize)]
struct PurgeSummary {
    environment_values: usize,
    guild_values: usize,
    guild_files: usize,
    legacy_files: usize,
}

#[derive(Serialize)]
struct KeyvaultManifest {
    schema: &'static str,
    created_at: u64,
    expires_at: u64,
    backup_id: String,
    prepared_by_discord_user_id: String,
    purge_scope: &'static str,
    planned_purge: PurgeSummary,
    confirmation: KeyvaultConfirmation,
    included_files: Vec<String>,
    included_process_environment: Vec<String>,
    notes: Vec<String>,
}

#[derive(Serialize)]
struct KeyvaultConfirmation {
    instruction: &'static str,
    proof: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct PendingPurge {
    version: u8,
    backup_id: String,
    prepared_by_discord_user_id: u64,
    created_at: u64,
    expires_at: u64,
    proof_sha256: String,
    backup_sources_sha256: String,
    purge_fingerprint_sha256: String,
    encrypted_backup_sha256: String,
}

struct StagedWrite {
    target: PathBuf,
    temporary: PathBuf,
}

pub async fn handle_keyvault(ctx: &Context, command: &serenity::all::CommandInteraction) {
    if !has_level_at_least(command.user.id.get(), 4) {
        command_error(ctx, command, "Error: `/keyvault` requires Witch rank.").await;
        return;
    }
    let Some((subcommand, _)) = subcommand_options(command) else {
        command_error(ctx, command, "Error: choose `prepare` or `confirm`.").await;
        return;
    };

    match subcommand {
        "prepare" => prepare_keyvault(ctx, command).await,
        "confirm" => confirm_keyvault(ctx, command).await,
        _ => command_error(ctx, command, "Error: unknown `/keyvault` operation.").await,
    }
}

fn keyvault_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

async fn prepare_keyvault(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let recipient_text = match option_trimmed(command, "recipient") {
        Some(recipient) => recipient,
        None => {
            command_error(ctx, command, "Error: an age X25519 recipient is required.").await;
            return;
        }
    };
    let recipient = match recipient_text.parse::<age::x25519::Recipient>() {
        Ok(recipient) => recipient,
        Err(_) => {
            command_error(
                ctx,
                command,
                "Error: recipient must be a valid age X25519 public key (`age1...`).",
            )
            .await;
            return;
        }
    };
    if command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .is_err()
    {
        return;
    }

    let _guard = keyvault_lock().lock().await;
    let now = unix_timestamp();
    let expires_at = now.saturating_add(KEYVAULT_TTL_SECS);
    let backup_id = match random_hex(16) {
        Ok(value) => value,
        Err(error) => {
            edit_keyvault_error(ctx, command, &error).await;
            return;
        }
    };
    let proof = match random_hex(32) {
        Ok(value) => value,
        Err(error) => {
            edit_keyvault_error(ctx, command, &error).await;
            return;
        }
    };

    let plan = match collect_purge_plan().await {
        Ok(plan) => plan,
        Err(error) => {
            edit_keyvault_error(
                ctx,
                command,
                &format!("Could not snapshot purge targets: {error}"),
            )
            .await;
            return;
        }
    };
    let mut backup = match collect_backup_sources().await {
        Ok(backup) => backup,
        Err(error) => {
            edit_keyvault_error(
                ctx,
                command,
                &format!("Could not collect credentials: {error}"),
            )
            .await;
            return;
        }
    };
    if let Err(error) = ensure_purge_targets_are_backed_up(&plan, &backup) {
        edit_keyvault_error(ctx, command, &error).await;
        return;
    }

    let summary = purge_summary(&plan);
    let proof_sha256 = sha256_hex(proof.as_bytes());
    let backup_sources_sha256 = backup_source_fingerprint(&backup);
    let included_files = backup.entries.keys().cloned().collect::<Vec<_>>();
    let manifest = KeyvaultManifest {
        schema: "pandora-keyvault/v1",
        created_at: now,
        expires_at,
        backup_id: backup_id.clone(),
        prepared_by_discord_user_id: command.user.id.get().to_string(),
        purge_scope: "legacy-upload-credentials-only",
        planned_purge: summary,
        confirmation: KeyvaultConfirmation {
            instruction: "Decrypt this archive and submit this proof with /keyvault confirm before expiry.",
            proof: proof.clone(),
        },
        included_files,
        included_process_environment: backup.process_environment_names.clone(),
        notes: backup.notes.clone(),
    };
    let mut plaintext_zip = match build_backup_zip(&mut backup, &manifest).await {
        Ok(bytes) => bytes,
        Err(error) => {
            edit_keyvault_error(
                ctx,
                command,
                &format!("Could not build backup archive: {error}"),
            )
            .await;
            return;
        }
    };
    drop(manifest);
    let mut proof_bytes = proof.into_bytes();
    proof_bytes.fill(0);
    if plaintext_zip.len() > MAX_BACKUP_SOURCE_BYTES {
        let size = plaintext_zip.len();
        plaintext_zip.fill(0);
        edit_keyvault_error(
            ctx,
            command,
            &format!(
                "Plaintext backup archive is {size} bytes, exceeding the {MAX_BACKUP_SOURCE_BYTES}-byte safety limit."
            ),
        )
        .await;
        return;
    }
    let encrypted = match encrypt_for_recipient(&recipient, &mut plaintext_zip) {
        Ok(encrypted) => encrypted,
        Err(error) => {
            edit_keyvault_error(ctx, command, &error).await;
            return;
        }
    };
    if encrypted.len() > MAX_ENCRYPTED_BACKUP_BYTES {
        edit_keyvault_error(
            ctx,
            command,
            &format!(
                "Encrypted backup is {} bytes, exceeding the Discord-safe {}-byte limit.",
                encrypted.len(),
                MAX_ENCRYPTED_BACKUP_BYTES,
            ),
        )
        .await;
        return;
    }

    let encrypted_sha256 = sha256_hex(&encrypted);
    let backup_path = encrypted_backup_path(&backup_id);
    match tokio::fs::symlink_metadata(&backup_path).await {
        Ok(_) => {
            edit_keyvault_error(
                ctx,
                command,
                "The generated backup id already exists; run prepare again.",
            )
            .await;
            return;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            edit_keyvault_error(
                ctx,
                command,
                &format!("Could not inspect the backup destination: {error}"),
            )
            .await;
            return;
        }
    }
    if let Err(error) = write_private_atomic(&backup_path, &encrypted).await {
        edit_keyvault_error(
            ctx,
            command,
            &format!("Could not retain the encrypted recovery copy: {error}"),
        )
        .await;
        return;
    }
    let pending = PendingPurge {
        version: KEYVAULT_VERSION,
        backup_id: backup_id.clone(),
        prepared_by_discord_user_id: command.user.id.get(),
        created_at: now,
        expires_at,
        proof_sha256,
        backup_sources_sha256,
        purge_fingerprint_sha256: plan.fingerprint.clone(),
        encrypted_backup_sha256: encrypted_sha256.clone(),
    };
    drop(plan);
    if let Err(error) = write_pending(&pending).await {
        edit_keyvault_error(
            ctx,
            command,
            &format!(
                "Encrypted backup was retained at `{}`, but the confirmation state could not be written: {error}. Nothing was purged.",
                backup_path.display(),
            ),
        )
        .await;
        return;
    }

    let encrypted_bytes = encrypted.len();
    let filename = format!("pandora-keyvault-{backup_id}.zip.age");
    let attachment = CreateAttachment::bytes(encrypted, filename.clone());
    println!(
        "[keyvault] prepared user={} backup_id={} files={} process_env={} purge_env_values={} purge_guild_values={} purge_legacy_files={} encrypted_bytes={} sha256={}",
        command.user.id.get(),
        backup_id,
        backup.entries.len(),
        backup.process_environment_names.len(),
        summary.environment_values,
        summary.guild_values,
        summary.legacy_files,
        encrypted_bytes,
        encrypted_sha256,
    );
    let content = format!(
        "Prepared encrypted key backup `{backup_id}`; **nothing has been purged yet**. Download `{filename}`, decrypt it with the matching age identity, and read `manifest.json`. Submit its one-time proof with `/keyvault confirm backup_id:{backup_id} proof:<proof>` before <t:{expires_at}:R>.\n\nThe encrypted recovery copy is also retained at `{}` with SHA-256 `{encrypted_sha256}`. It contains {} file(s) and {} visible sensitive process variable(s). The confirmed purge would blank {} environment value(s), blank {} guild Drive field(s), and remove {} legacy credential file(s). Confirm only after successful Lumiere smoke tests and an offline ciphertext copy. Cloudflare Worker secret bindings and variables visible only inside the separate cloudflared container cannot be exported by Pandora.",
        backup_path.display(),
        backup.entries.len(),
        backup.process_environment_names.len(),
        summary.environment_values,
        summary.guild_values,
        summary.legacy_files,
    );
    command
        .edit_response(
            ctx,
            EditInteractionResponse::new()
                .content(content)
                .new_attachment(attachment),
        )
        .await
        .ok();
}

async fn confirm_keyvault(ctx: &Context, command: &serenity::all::CommandInteraction) {
    let backup_id = match option_trimmed(command, "backup_id") {
        Some(value) if valid_hex(&value, 32) => value,
        _ => {
            command_error(
                ctx,
                command,
                "Error: `backup_id` must be the 32-character id from the decrypted manifest.",
            )
            .await;
            return;
        }
    };
    let proof = match option_trimmed(command, "proof") {
        Some(value) if valid_hex(&value, 64) => value,
        _ => {
            command_error(
                ctx,
                command,
                "Error: `proof` must be the 64-character value from the decrypted manifest.",
            )
            .await;
            return;
        }
    };
    if command
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
        .is_err()
    {
        return;
    }

    let _guard = keyvault_lock().lock().await;
    let mut pending = match read_pending().await {
        Ok(pending) => pending,
        Err(error) => {
            edit_keyvault_error(ctx, command, &error).await;
            return;
        }
    };
    if pending.version != KEYVAULT_VERSION
        || pending.backup_id != backup_id
        || pending.prepared_by_discord_user_id != command.user.id.get()
    {
        edit_keyvault_error(
            ctx,
            command,
            "The pending backup does not match this id and Discord operator.",
        )
        .await;
        return;
    }
    if unix_timestamp() > pending.expires_at {
        edit_keyvault_error(
            ctx,
            command,
            "The confirmation window expired. Run `/keyvault prepare` again; nothing was purged.",
        )
        .await;
        return;
    }
    let supplied_proof_hash = sha256_hex(proof.as_bytes());
    let mut proof_bytes = proof.into_bytes();
    proof_bytes.fill(0);
    if !constant_time_eq(
        supplied_proof_hash.as_bytes(),
        pending.proof_sha256.as_bytes(),
    ) {
        edit_keyvault_error(
            ctx,
            command,
            "The decrypted confirmation proof is incorrect.",
        )
        .await;
        return;
    }

    let backup_path = encrypted_backup_path(&backup_id);
    let encrypted = match read_regular_file_bounded(&backup_path, MAX_ENCRYPTED_BACKUP_BYTES).await
    {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            edit_keyvault_error(
                ctx,
                command,
                "The retained encrypted recovery copy is missing; refusing to purge.",
            )
            .await;
            return;
        }
        Err(error) => {
            edit_keyvault_error(ctx, command, &error).await;
            return;
        }
    };
    if !constant_time_eq(
        sha256_hex(&encrypted).as_bytes(),
        pending.encrypted_backup_sha256.as_bytes(),
    ) {
        edit_keyvault_error(
            ctx,
            command,
            "The retained encrypted recovery copy failed its digest check; refusing to purge.",
        )
        .await;
        return;
    }

    let current_backup = match collect_backup_sources().await {
        Ok(backup) => backup,
        Err(error) => {
            edit_keyvault_error(
                ctx,
                command,
                &format!("Could not re-read credential sources: {error}"),
            )
            .await;
            return;
        }
    };
    let current_backup_fingerprint = backup_source_fingerprint(&current_backup);
    drop(current_backup);
    if !constant_time_eq(
        current_backup_fingerprint.as_bytes(),
        pending.backup_sources_sha256.as_bytes(),
    ) {
        edit_keyvault_error(
            ctx,
            command,
            "A backed-up credential source changed after prepare. Refusing to purge; run `/keyvault prepare` again so every retained and removed key is captured.",
        )
        .await;
        return;
    }

    let plan = match collect_purge_plan().await {
        Ok(plan) => plan,
        Err(error) => {
            edit_keyvault_error(
                ctx,
                command,
                &format!("Could not verify purge targets: {error}"),
            )
            .await;
            return;
        }
    };
    if !constant_time_eq(
        plan.fingerprint.as_bytes(),
        pending.purge_fingerprint_sha256.as_bytes(),
    ) {
        edit_keyvault_error(
            ctx,
            command,
            "Legacy credential files changed after the backup. Refusing to purge unbacked data; run `/keyvault prepare` again.",
        )
        .await;
        return;
    }
    let summary = purge_summary(&plan);
    let errors = execute_purge(&plan).await;
    if !errors.is_empty() {
        if let (Ok(remaining), Ok(current_backup)) =
            (collect_purge_plan().await, collect_backup_sources().await)
        {
            pending.purge_fingerprint_sha256 = remaining.fingerprint;
            pending.backup_sources_sha256 = backup_source_fingerprint(&current_backup);
            pending.expires_at = unix_timestamp().saturating_add(KEYVAULT_TTL_SECS);
            write_pending(&pending).await.ok();
        }
        command
            .edit_response(
                ctx,
                EditInteractionResponse::new().content(format!(
                    "The legacy credential purge was only partially completed. The encrypted backup remains at `{}`. Fix the filesystem errors and retry the same confirmation, or prepare a new backup. Errors:\n{}",
                    backup_path.display(),
                    format_error_list(&errors),
                )),
            )
            .await
            .ok();
        return;
    }

    let pending_cleanup = tokio::fs::remove_file(PENDING_PATH).await.err();
    println!(
        "[keyvault] purged user={} backup_id={} env_values={} guild_values={} guild_files={} legacy_files={} backup_sha256={}",
        command.user.id.get(),
        backup_id,
        summary.environment_values,
        summary.guild_values,
        summary.guild_files,
        summary.legacy_files,
        pending.encrypted_backup_sha256,
    );
    let cleanup_note = pending_cleanup
        .map(|error| format!(" Pending-state cleanup failed: {error}."))
        .unwrap_or_default();
    command
        .edit_response(
            ctx,
            EditInteractionResponse::new().content(format!(
                "Legacy upload credentials were logically purged: {} global/root environment value(s), {} guild Drive field(s) across {} guild file(s), and {} legacy credential file(s) (`gdrive_env.pandora` or local Drive profile JSON). Operational Discord, Lumiere, Forgejo, distribution, session, and HTTP API credentials were retained. The encrypted recovery copy remains at `{}` with SHA-256 `{}`.{} Filesystem journals, snapshots, old images, external backups, and the current process environment may retain prior values; inspect `runtime/process-environment.json`, recreate the container from scrubbed deployment inputs, and rotate the migrated provider credentials after retesting Lumiere.",
                summary.environment_values,
                summary.guild_values,
                summary.guild_files,
                summary.legacy_files,
                backup_path.display(),
                pending.encrypted_backup_sha256,
                cleanup_note,
            )),
        )
        .await
        .ok();
}

async fn collect_backup_sources() -> Result<BackupCollection, String> {
    let mut candidates = BTreeMap::<String, (PathBuf, String)>::new();
    discover_backup_candidates(Path::new("DB"), BackupScanKind::Database, &mut candidates).await?;
    discover_backup_candidates(
        Path::new("work"),
        BackupScanKind::Historical,
        &mut candidates,
    )
    .await?;
    add_explicit_candidate(
        Path::new("env.pandora"),
        "files/root/env.pandora",
        &mut candidates,
    )
    .await?;
    add_explicit_candidate(Path::new(".env"), "files/root/.env", &mut candidates).await?;
    add_explicit_candidate(
        Path::new("/repo/env.pandora"),
        "files/repo/env.pandora",
        &mut candidates,
    )
    .await?;
    add_explicit_candidate(Path::new("/repo/.env"), "files/repo/.env", &mut candidates).await?;
    for (path, archive_path) in [
        (
            "cloudflare/lumiere-broker/drive-profiles.json",
            "files/root/cloudflare/lumiere-broker/drive-profiles.json",
        ),
        (
            "/repo/cloudflare/lumiere-broker/drive-profiles.json",
            "files/repo/cloudflare/lumiere-broker/drive-profiles.json",
        ),
        (
            "cloudflare/lumiere-broker/.dev.vars",
            "files/root/cloudflare/lumiere-broker/.dev.vars",
        ),
        (
            "/repo/cloudflare/lumiere-broker/.dev.vars",
            "files/repo/cloudflare/lumiere-broker/.dev.vars",
        ),
        (
            "cloudflare/lumiere-broker/wrangler.toml",
            "files/root/cloudflare/lumiere-broker/wrangler.toml",
        ),
        (
            "/repo/cloudflare/lumiere-broker/wrangler.toml",
            "files/repo/cloudflare/lumiere-broker/wrangler.toml",
        ),
    ] {
        add_explicit_candidate(Path::new(path), archive_path, &mut candidates).await?;
    }
    discover_docker_secrets(&mut candidates).await?;

    let mut entries = BTreeMap::new();
    let mut total = 0usize;
    for (_, (path, archive_path)) in candidates {
        let bytes = read_regular_file(&path)
            .await?
            .ok_or_else(|| format!("credential source disappeared: {}", path.display()))?;
        if bytes.len() > MAX_BACKUP_FILE_BYTES {
            return Err(format!(
                "credential source {} is {} bytes, exceeding the per-file limit",
                path.display(),
                bytes.len(),
            ));
        }
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| "credential backup size overflow".to_string())?;
        if total > MAX_BACKUP_SOURCE_BYTES {
            return Err(format!(
                "credential sources exceed the {}-byte backup limit",
                MAX_BACKUP_SOURCE_BYTES,
            ));
        }
        if entries.contains_key(&archive_path) {
            return Err(format!("duplicate backup archive path `{archive_path}`"));
        }
        entries.insert(
            archive_path,
            BackupEntry {
                source_key: Some(source_key(&path)),
                bytes: SensitiveBytes::new(bytes),
            },
        );
    }

    let process_environment = sensitive_process_environment();
    let process_environment_names = process_environment.keys().cloned().collect::<Vec<_>>();
    if !process_environment.is_empty() {
        let bytes = serde_json::to_vec_pretty(&process_environment).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_BACKUP_FILE_BYTES {
            return Err(
                "sensitive process environment exceeds the per-file backup limit".to_string(),
            );
        }
        total = total
            .checked_add(bytes.len())
            .ok_or_else(|| "credential backup size overflow".to_string())?;
        if total > MAX_BACKUP_SOURCE_BYTES {
            return Err(format!(
                "credential sources exceed the {}-byte backup limit",
                MAX_BACKUP_SOURCE_BYTES,
            ));
        }
        entries.insert(
            "runtime/process-environment.json".to_string(),
            BackupEntry {
                source_key: None,
                bytes: SensitiveBytes::new(bytes),
            },
        );
    }
    entries.insert(
        "README.txt".to_string(),
        BackupEntry {
            source_key: None,
            bytes: SensitiveBytes::new(
                b"Pandora encrypted key backup. Keep this archive offline. manifest.json contains the one-time purge proof. Restore files only after reviewing paths and rotate reusable credentials after migration. Cloudflare Worker secret bindings are not exportable through the Worker API.\n".to_vec(),
            ),
        },
    );

    Ok(BackupCollection {
        entries,
        process_environment_names,
        notes: vec![
            "The complete env.pandora and api.pandora files are included when readable, so unknown keys added through /touchapi are preserved.".to_string(),
            "Cloudflare Worker secret bindings are intentionally unreadable and are not included.".to_string(),
            "This is a bounded backup of known Pandora, Compose, Docker-secret, and runtime credential sources, not a whole-filesystem secret scanner; review the included file list before confirmation.".to_string(),
            "A Tunnel token visible only inside the separate cloudflared container is not readable by pndc; it is included only when present in a readable Compose .env file or pndc process variable.".to_string(),
            "Confirm the purge only after successful Lumiere Drive/provider smoke tests and an offline ciphertext copy.".to_string(),
            "The purge phase removes only legacy upload-provider credentials, historical gdrive_env.pandora files, and readable local drive-profiles.json copies; every other backed-up credential remains on the VDS.".to_string(),
            "Already inherited process environment values cannot be removed from the parent container configuration; inspect runtime/process-environment.json, scrub their deployment source, and recreate the container.".to_string(),
            "Files mounted under /run/secrets are backed up but never modified; remove legacy provider Docker secrets through the container orchestrator.".to_string(),
        ],
    })
}

#[derive(Clone, Copy)]
enum BackupScanKind {
    Database,
    Historical,
}

async fn discover_backup_candidates(
    root: &Path,
    kind: BackupScanKind,
    candidates: &mut BTreeMap<String, (PathBuf, String)>,
) -> Result<(), String> {
    let metadata = match tokio::fs::symlink_metadata(root).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    let mut stack = vec![root.to_path_buf()];
    let mut scanned = 0usize;
    while let Some(directory) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&directory)
            .await
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| format!("{}: {error}", directory.display()))?
        {
            scanned += 1;
            if scanned > MAX_SCANNED_ENTRIES {
                return Err(format!(
                    "credential scan exceeded {MAX_SCANNED_ENTRIES} filesystem entries"
                ));
            }
            let path = entry.path();
            let file_type = entry
                .file_type()
                .await
                .map_err(|error| format!("{}: {error}", path.display()))?;
            if file_type.is_symlink() {
                if backup_path_matches(&path, kind) {
                    return Err(format!(
                        "refusing symlinked credential source {}",
                        path.display()
                    ));
                }
                continue;
            }
            if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file() && backup_path_matches(&path, kind) {
                let relative = path.strip_prefix(root).unwrap_or(&path);
                let archive_path = format!(
                    "files/{}/{}",
                    root.file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("root"),
                    normalized_archive_path(relative),
                );
                candidates.insert(source_key(&path), (path, archive_path));
            }
        }
    }
    Ok(())
}

fn backup_path_matches(path: &Path, kind: BackupScanKind) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match kind {
        BackupScanKind::Historical => matches!(name.as_str(), "gdrive_env.pandora" | "env.pandora"),
        BackupScanKind::Database => {
            if matches!(
                name.as_str(),
                "env.pandora" | "api.pandora" | "gdrive_env.pandora" | "meta.pandora"
            ) || name.ends_with(".session")
            {
                return true;
            }
            if path
                .components()
                .any(|component| component.as_os_str() == "smartcode_drive")
            {
                return true;
            }
            let in_global_environment = path
                .to_string_lossy()
                .replace('\\', "/")
                .contains("DB/config/global/environment/");
            in_global_environment
                && ["token", "secret", "credential", "password"]
                    .iter()
                    .any(|needle| name.contains(needle))
        }
    }
}

async fn add_explicit_candidate(
    path: &Path,
    archive_path: &str,
    candidates: &mut BTreeMap<String, (PathBuf, String)>,
) -> Result<(), String> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "refusing symlinked credential source {}",
            path.display()
        ));
    }
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    candidates.insert(
        source_key(path),
        (path.to_path_buf(), archive_path.to_string()),
    );
    Ok(())
}

async fn discover_docker_secrets(
    candidates: &mut BTreeMap<String, (PathBuf, String)>,
) -> Result<(), String> {
    let root = Path::new("/run/secrets");
    let mut entries = match tokio::fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        let path = entry.path();
        let file_type = entry.file_type().await.map_err(|e| e.to_string())?;
        if file_type.is_symlink() {
            return Err(format!(
                "refusing symlinked Docker secret {}",
                path.display()
            ));
        }
        if !file_type.is_file() {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| "Docker secret has a non-UTF-8 filename".to_string())?
            .to_string();
        candidates.insert(
            source_key(&path),
            (
                path,
                format!("files/docker-secrets/{}", safe_archive_component(&name)),
            ),
        );
    }
    Ok(())
}

fn sensitive_process_environment() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(name, value)| !value.is_empty() && sensitive_process_environment_name(name))
        .collect()
}

fn sensitive_process_environment_name(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    if legacy_dotenv_key(&name)
        || [
            "DISCORD_TOKEN",
            "LUMIERE_BROKER_TOKEN",
            "FORGEJO_API_KEY",
            "ANISUB",
            "ANIMECIX",
            "ANIMECIX_PASSWORD",
            "OPENANIME_PASSWORD",
            "ANIZM_PASSWORD",
            "AKIRA_TOKEN",
        ]
        .contains(&name.as_str())
    {
        return true;
    }
    let scoped = name.starts_with("PANDORA_")
        || name.starts_with("CLOUDFLARE_")
        || name.starts_with("CF_")
        || name.starts_with("WRANGLER_")
        || name.starts_with("TUNNEL_");
    scoped
        && [
            "TOKEN",
            "SECRET",
            "PASSWORD",
            "PASSPHRASE",
            "API_KEY",
            "PRIVATE_KEY",
            "CREDENTIAL",
        ]
        .iter()
        .any(|needle| name.contains(needle))
}

async fn build_backup_zip(
    backup: &mut BackupCollection,
    manifest: &KeyvaultManifest,
) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let result = async {
        let mut writer = async_zip::base::write::ZipFileWriter::new(&mut out);
        let mut manifest_bytes = serde_json::to_vec_pretty(manifest).map_err(|e| e.to_string())?;
        let manifest_entry = async_zip::ZipEntryBuilder::new(
            "manifest.json".to_string().into(),
            async_zip::Compression::Deflate,
        );
        let manifest_result = writer
            .write_entry_whole(manifest_entry, &manifest_bytes)
            .await
            .map_err(|e| e.to_string());
        manifest_bytes.fill(0);
        manifest_result?;

        for (archive_path, entry) in &mut backup.entries {
            let zip_entry = async_zip::ZipEntryBuilder::new(
                archive_path.clone().into(),
                async_zip::Compression::Deflate,
            );
            writer
                .write_entry_whole(zip_entry, entry.bytes.as_slice())
                .await
                .map_err(|e| e.to_string())?;
            entry.bytes.0.fill(0);
        }
        writer.close().await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
    .await;
    if let Err(error) = result {
        out.fill(0);
        return Err(error);
    }
    Ok(out)
}

fn encrypt_for_recipient(
    recipient: &age::x25519::Recipient,
    plaintext: &mut [u8],
) -> Result<Vec<u8>, String> {
    let result = (|| {
        let encryptor =
            age::Encryptor::with_recipients(std::iter::once(recipient as &dyn age::Recipient))
                .map_err(|_| "Keyvault encryption initialization failed.".to_string())?;
        let mut encrypted = Vec::new();
        let mut writer = encryptor
            .wrap_output(&mut encrypted)
            .map_err(|_| "Keyvault encryption failed.".to_string())?;
        writer
            .write_all(plaintext)
            .map_err(|_| "Keyvault encryption failed.".to_string())?;
        writer
            .finish()
            .map_err(|_| "Keyvault encryption failed.".to_string())?;
        Ok(encrypted)
    })();
    plaintext.fill(0);
    result
}

async fn collect_purge_plan() -> Result<PurgePlan, String> {
    let mut writes = BTreeMap::<String, PlannedWrite>::new();
    let mut deletes = BTreeMap::<String, PlannedDelete>::new();
    let mut seen_environment_files = BTreeSet::new();

    for path in [
        PathBuf::from(ENV_PATH),
        PathBuf::from("env.pandora"),
        PathBuf::from("/repo/env.pandora"),
    ] {
        let Some(original) = read_regular_file(&path).await? else {
            continue;
        };
        let identity = canonical_source_key(&path).await?;
        if !seen_environment_files.insert(identity) {
            continue;
        }
        let (replacement, values_removed) = scrub_legacy_environment(&original)?;
        if values_removed > 0 {
            writes.insert(
                source_key(&path),
                PlannedWrite {
                    path,
                    original: SensitiveBytes::new(original),
                    replacement: SensitiveBytes::new(replacement),
                    values_removed,
                    kind: PurgeWriteKind::Environment,
                },
            );
        }
    }

    for path in [
        PathBuf::from(".env"),
        PathBuf::from("/repo/.env"),
        PathBuf::from("cloudflare/lumiere-broker/.dev.vars"),
        PathBuf::from("/repo/cloudflare/lumiere-broker/.dev.vars"),
    ] {
        let Some(original) = read_regular_file(&path).await? else {
            continue;
        };
        let identity = canonical_source_key(&path).await?;
        if !seen_environment_files.insert(identity) {
            continue;
        }
        let (replacement, values_removed) = scrub_legacy_dotenv(&original)?;
        if values_removed > 0 {
            writes.insert(
                source_key(&path),
                PlannedWrite {
                    path,
                    original: SensitiveBytes::new(original),
                    replacement: SensitiveBytes::new(replacement),
                    values_removed,
                    kind: PurgeWriteKind::Environment,
                },
            );
        }
    }

    let config_root = Path::new("DB").join("config");
    let mut guild_entries = match tokio::fs::read_dir(&config_root).await {
        Ok(entries) => Some(entries),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("{}: {error}", config_root.display())),
    };
    if let Some(entries) = guild_entries.as_mut() {
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            let Some(name) = entry.file_name().to_str().map(str::to_string) else {
                continue;
            };
            if name.parse::<u64>().is_err() {
                continue;
            }
            let path = entry.path().join("meta.pandora");
            let Some(original) = read_regular_file(&path).await? else {
                continue;
            };
            let (replacement, values_removed) = scrub_guild_drive_fields(&original)?;
            if values_removed > 0 {
                writes.insert(
                    source_key(&path),
                    PlannedWrite {
                        path,
                        original: SensitiveBytes::new(original),
                        replacement: SensitiveBytes::new(replacement),
                        values_removed,
                        kind: PurgeWriteKind::Guild,
                    },
                );
            }
        }
    }

    for path in [
        PathBuf::from("cloudflare/lumiere-broker/drive-profiles.json"),
        PathBuf::from("/repo/cloudflare/lumiere-broker/drive-profiles.json"),
    ] {
        let Some(original) = read_regular_file(&path).await? else {
            continue;
        };
        let identity = canonical_source_key(&path).await?;
        if deletes.contains_key(&identity) {
            continue;
        }
        deletes.insert(
            identity,
            PlannedDelete {
                path,
                original: SensitiveBytes::new(original),
            },
        );
    }

    for root in [Path::new("DB"), Path::new("work")] {
        for path in find_named_regular_files(root, "gdrive_env.pandora").await? {
            let original = read_regular_file(&path)
                .await?
                .ok_or_else(|| format!("purge target disappeared: {}", path.display()))?;
            deletes.insert(
                source_key(&path),
                PlannedDelete {
                    path,
                    original: SensitiveBytes::new(original),
                },
            );
        }
    }

    let writes = writes.into_values().collect::<Vec<_>>();
    let deletes = deletes.into_values().collect::<Vec<_>>();
    let fingerprint = purge_fingerprint(&writes, &deletes);
    Ok(PurgePlan {
        writes,
        deletes,
        fingerprint,
    })
}

async fn find_named_regular_files(root: &Path, target: &str) -> Result<Vec<PathBuf>, String> {
    let metadata = match tokio::fs::symlink_metadata(root).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", root.display())),
    };
    if !metadata.is_dir() {
        return Ok(Vec::new());
    }
    let mut stack = vec![root.to_path_buf()];
    let mut found = Vec::new();
    let mut scanned = 0usize;
    while let Some(directory) = stack.pop() {
        let mut entries = tokio::fs::read_dir(&directory)
            .await
            .map_err(|error| format!("{}: {error}", directory.display()))?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            scanned += 1;
            if scanned > MAX_SCANNED_ENTRIES {
                return Err(format!("purge scan exceeded {MAX_SCANNED_ENTRIES} entries"));
            }
            let path = entry.path();
            let file_type = entry.file_type().await.map_err(|e| e.to_string())?;
            if file_type.is_symlink() {
                if path.file_name().and_then(|name| name.to_str()) == Some(target) {
                    return Err(format!(
                        "refusing symlinked purge target {}",
                        path.display()
                    ));
                }
            } else if file_type.is_dir() {
                stack.push(path);
            } else if file_type.is_file()
                && path.file_name().and_then(|name| name.to_str()) == Some(target)
            {
                found.push(path);
            }
        }
    }
    found.sort();
    Ok(found)
}

fn scrub_legacy_environment(original: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let text = std::str::from_utf8(original).map_err(|_| "env.pandora is not UTF-8".to_string())?;
    let keyed = text
        .lines()
        .map(str::trim)
        .any(|line| !line.is_empty() && !line.starts_with('#') && line.contains(ENV_SEP));
    let mut lines = text
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect::<Vec<_>>();
    let mut removed = 0usize;
    if keyed {
        for line in &mut lines {
            let Some((name, value)) = line.split_once(ENV_SEP) else {
                continue;
            };
            if legacy_environment_key(name.trim()) && !value.trim().is_empty() {
                *line = format!("{}{}", name, ENV_SEP);
                removed += 1;
            }
        }
    } else {
        for index in LEGACY_ENV_LINE_INDICES {
            if let Some(value) = lines.get_mut(*index)
                && !value.trim().is_empty()
            {
                value.clear();
                removed += 1;
            }
        }
    }
    if removed == 0 {
        return Ok((original.to_vec(), 0));
    }
    Ok((text_lines_with_newline(lines), removed))
}

fn legacy_environment_key(name: &str) -> bool {
    LEGACY_ENV_KEYS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

fn scrub_legacy_dotenv(original: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let text =
        std::str::from_utf8(original).map_err(|_| "Compose .env is not UTF-8".to_string())?;
    let mut lines = text
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect::<Vec<_>>();
    let mut removed = 0usize;
    for line in &mut lines {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if legacy_dotenv_key(name.trim()) && !value.trim().is_empty() {
            *line = format!("{}=", name);
            removed += 1;
        }
    }
    if removed == 0 {
        return Ok((original.to_vec(), 0));
    }
    Ok((text_lines_with_newline(lines), removed))
}

fn legacy_dotenv_key(name: &str) -> bool {
    legacy_environment_key(name)
        || [
            "DOODSTREAM_API_KEY",
            "LULUSTREAM_API_KEY",
            "LULU_API_KEY",
            "VOE_API_KEY",
            "VOESX_API_KEY",
            "ABYSS_API_KEY",
            "UQLOAD_API_KEY",
            "LUMIERE_DRIVE_PROFILES",
        ]
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(name))
}

fn scrub_guild_drive_fields(original: &[u8]) -> Result<(Vec<u8>, usize), String> {
    let text =
        std::str::from_utf8(original).map_err(|_| "guild meta.pandora is not UTF-8".to_string())?;
    let mut lines = text
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect::<Vec<_>>();
    let mut removed = 0usize;
    for index in LEGACY_GUILD_LINE_INDICES {
        if let Some(value) = lines.get_mut(*index)
            && !value.trim().is_empty()
        {
            value.clear();
            removed += 1;
        }
    }
    if removed == 0 {
        return Ok((original.to_vec(), 0));
    }
    Ok((text_lines_with_newline(lines), removed))
}

fn text_lines_with_newline(lines: Vec<String>) -> Vec<u8> {
    let mut output = lines.join("\n");
    output.push('\n');
    output.into_bytes()
}

fn backup_source_fingerprint(backup: &BackupCollection) -> String {
    let mut hasher = Sha256::new();
    for (archive_path, entry) in &backup.entries {
        hasher.update(b"B");
        hasher.update((archive_path.len() as u64).to_be_bytes());
        hasher.update(archive_path.as_bytes());
        let source = entry.source_key.as_deref().unwrap_or("");
        hasher.update((source.len() as u64).to_be_bytes());
        hasher.update(source.as_bytes());
        hasher.update((entry.bytes.as_slice().len() as u64).to_be_bytes());
        hasher.update(entry.bytes.as_slice());
    }
    hex_bytes(&hasher.finalize())
}

fn purge_fingerprint(writes: &[PlannedWrite], deletes: &[PlannedDelete]) -> String {
    let mut hasher = Sha256::new();
    for write in writes {
        fingerprint_part(&mut hasher, b'W', &write.path, write.original.as_slice());
    }
    for delete in deletes {
        fingerprint_part(&mut hasher, b'D', &delete.path, delete.original.as_slice());
    }
    hex_bytes(&hasher.finalize())
}

fn fingerprint_part(hasher: &mut Sha256, kind: u8, path: &Path, bytes: &[u8]) {
    let path = source_key(path);
    hasher.update([kind]);
    hasher.update((path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}

fn ensure_purge_targets_are_backed_up(
    plan: &PurgePlan,
    backup: &BackupCollection,
) -> Result<(), String> {
    for (path, bytes) in plan
        .writes
        .iter()
        .map(|write| (&write.path, write.original.as_slice()))
        .chain(
            plan.deletes
                .iter()
                .map(|delete| (&delete.path, delete.original.as_slice())),
        )
    {
        let key = source_key(path);
        let backed_up = backup.entries.values().any(|entry| {
            entry.source_key.as_deref() == Some(key.as_str()) && entry.bytes.as_slice() == bytes
        });
        if !backed_up {
            return Err(format!(
                "purge target {} was not captured exactly; refusing to prepare",
                path.display()
            ));
        }
    }
    Ok(())
}

fn purge_summary(plan: &PurgePlan) -> PurgeSummary {
    let mut summary = PurgeSummary {
        legacy_files: plan.deletes.len(),
        ..Default::default()
    };
    for write in &plan.writes {
        match write.kind {
            PurgeWriteKind::Environment => summary.environment_values += write.values_removed,
            PurgeWriteKind::Guild => {
                summary.guild_values += write.values_removed;
                summary.guild_files += 1;
            }
        }
    }
    summary
}

async fn execute_purge(plan: &PurgePlan) -> Vec<String> {
    let mut staged = Vec::new();
    for write in &plan.writes {
        match stage_private_replacement(&write.path, write.replacement.as_slice()).await {
            Ok(value) => staged.push(value),
            Err(error) => {
                cleanup_staged(&staged).await;
                return vec![error];
            }
        }
    }

    for write in &plan.writes {
        match read_regular_file(&write.path).await {
            Ok(Some(current)) if current == write.original.as_slice() => {}
            Ok(_) => {
                cleanup_staged(&staged).await;
                return vec![format!(
                    "{} changed during purge staging; no staged replacements were committed",
                    write.path.display()
                )];
            }
            Err(error) => {
                cleanup_staged(&staged).await;
                return vec![error];
            }
        }
    }
    for delete in &plan.deletes {
        match read_regular_file(&delete.path).await {
            Ok(Some(current)) if current == delete.original.as_slice() => {}
            Ok(_) => {
                cleanup_staged(&staged).await;
                return vec![format!(
                    "{} changed during purge staging; no staged replacements were committed",
                    delete.path.display()
                )];
            }
            Err(error) => {
                cleanup_staged(&staged).await;
                return vec![error];
            }
        }
    }

    let mut errors = Vec::new();
    for (index, staged_write) in staged.iter().enumerate() {
        if let Err(error) = tokio::fs::rename(&staged_write.temporary, &staged_write.target).await {
            errors.push(format!(
                "could not replace {}: {error}",
                staged_write.target.display()
            ));
            tokio::fs::remove_file(&staged_write.temporary).await.ok();
            for remaining in staged.iter().skip(index + 1) {
                tokio::fs::remove_file(&remaining.temporary).await.ok();
            }
            break;
        }
    }
    if errors.is_empty() {
        for delete in &plan.deletes {
            if let Err(error) = tokio::fs::remove_file(&delete.path).await {
                errors.push(format!(
                    "could not remove {}: {error}",
                    delete.path.display()
                ));
            }
        }
    }
    errors
}

async fn stage_private_replacement(path: &Path, bytes: &[u8]) -> Result<StagedWrite, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("{} has an invalid filename", path.display()))?;
    let suffix = random_hex(8)?;
    let temporary = parent.join(format!(".{name}.keyvault-{suffix}"));
    write_private_new(&temporary, bytes).await?;
    Ok(StagedWrite {
        target: path.to_path_buf(),
        temporary,
    })
}

async fn cleanup_staged(staged: &[StagedWrite]) {
    for write in staged {
        tokio::fs::remove_file(&write.temporary).await.ok();
    }
}

async fn write_pending(pending: &PendingPurge) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(pending).map_err(|e| e.to_string())?;
    write_private_atomic(Path::new(PENDING_PATH), &bytes).await
}

async fn read_pending() -> Result<PendingPurge, String> {
    let bytes = read_regular_file(Path::new(PENDING_PATH))
        .await?
        .ok_or_else(|| {
            "No keyvault purge is pending. Run `/keyvault prepare` first.".to_string()
        })?;
    serde_json::from_slice(&bytes).map_err(|_| "Pending keyvault state is invalid.".to_string())
}

async fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("{} has no parent", path.display()))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|e| e.to_string())?;
    set_directory_private(parent).await?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "invalid private filename".to_string())?;
    let temporary = parent.join(format!(".{name}.{}", random_hex(8)?));
    write_private_new(&temporary, bytes).await?;
    if let Err(error) = tokio::fs::rename(&temporary, path).await {
        tokio::fs::remove_file(&temporary).await.ok();
        return Err(error.to_string());
    }
    Ok(())
}

async fn write_private_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let result = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        set_file_private(path).await?;
        file.write_all(bytes)
            .await
            .map_err(|e| format!("{}: {e}", path.display()))?;
        file.flush().await.map_err(|e| e.to_string())?;
        file.sync_all().await.map_err(|e| e.to_string())?;
        Ok::<(), String>(())
    }
    .await;
    if result.is_err() {
        tokio::fs::remove_file(path).await.ok();
    }
    result
}

#[cfg(unix)]
async fn set_file_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
async fn set_file_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
async fn set_directory_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(|e| e.to_string())
}

#[cfg(not(unix))]
async fn set_directory_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

async fn read_regular_file(path: &Path) -> Result<Option<Vec<u8>>, String> {
    read_regular_file_bounded(path, MAX_BACKUP_FILE_BYTES).await
}

async fn read_regular_file_bounded(
    path: &Path,
    maximum_bytes: usize,
) -> Result<Option<Vec<u8>>, String> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if metadata.file_type().is_symlink() {
        return Err(format!("refusing symlinked file {}", path.display()));
    }
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if metadata.len() > maximum_bytes as u64 {
        return Err(format!(
            "{} exceeds the per-file size limit",
            path.display()
        ));
    }
    tokio::fs::read(path)
        .await
        .map(Some)
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn encrypted_backup_path(backup_id: &str) -> PathBuf {
    Path::new(KEYVAULT_DIR).join(format!("pandora-keyvault-{backup_id}.zip.age"))
}

fn source_key(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

async fn canonical_source_key(path: &Path) -> Result<String, String> {
    tokio::fs::canonicalize(path)
        .await
        .map(|path| source_key(&path))
        .map_err(|error| format!("{}: {error}", path.display()))
}

fn normalized_archive_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| component.as_os_str().to_str())
        .map(safe_archive_component)
        .collect::<Vec<_>>()
        .join("/")
}

fn safe_archive_component(value: &str) -> String {
    let filtered = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    if filtered.is_empty() || filtered == "." || filtered == ".." {
        "unnamed".to_string()
    } else {
        filtered
    }
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut random = vec![0u8; bytes];
    getrandom::getrandom(&mut random).map_err(|e| format!("entropy source failed: {e}"))?;
    let output = hex_bytes(&random);
    random.fill(0);
    Ok(output)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex_bytes(&Sha256::digest(bytes))
}

fn hex_bytes(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn valid_hex(value: &str, length: usize) -> bool {
    value.len() == length && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn unix_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn format_error_list(errors: &[String]) -> String {
    let mut lines = errors
        .iter()
        .take(8)
        .map(|error| format!("- {}", error.chars().take(180).collect::<String>()))
        .collect::<Vec<_>>();
    if errors.len() > lines.len() {
        lines.push(format!(
            "- …and {} more error(s)",
            errors.len() - lines.len()
        ));
    }
    lines.join("\n")
}

async fn edit_keyvault_error(
    ctx: &Context,
    command: &serenity::all::CommandInteraction,
    error: &str,
) {
    command
        .edit_response(
            ctx,
            EditInteractionResponse::new().content(format!("Keyvault error: {error}")),
        )
        .await
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;
    use pandora_toolchain::lib::env::standard::LUMIERE_BROKER_TOKEN;
    use std::io::Read;

    #[test]
    fn keyed_environment_scrub_removes_only_legacy_upload_values() {
        let input = format!(
            "discord_token{ENV_SEP}discord\n{CLIENT_ID}{ENV_SEP}client\n{DOODSTREAM}{ENV_SEP}dood\n{LUMIERE_BROKER_TOKEN}{ENV_SEP}broker\ncustom_key{ENV_SEP}custom\n"
        );
        let (scrubbed, removed) = scrub_legacy_environment(input.as_bytes()).unwrap();
        let scrubbed = String::from_utf8(scrubbed).unwrap();
        assert_eq!(removed, 2);
        assert!(scrubbed.contains(&format!("discord_token{ENV_SEP}discord")));
        assert!(scrubbed.contains(&format!("{CLIENT_ID}{ENV_SEP}\n")));
        assert!(scrubbed.contains(&format!("{DOODSTREAM}{ENV_SEP}\n")));
        assert!(scrubbed.contains(&format!("{LUMIERE_BROKER_TOKEN}{ENV_SEP}broker")));
        assert!(scrubbed.contains(&format!("custom_key{ENV_SEP}custom")));
    }

    #[test]
    fn compose_environment_scrub_keeps_tunnel_and_broker_tokens() {
        let input = "TUNNEL_TOKEN=tunnel\nPANDORA_GITSYNC_TOKEN=git\nDOODSTREAM_API_KEY=dood\nLUMIERE_DRIVE_PROFILES={secret}\n";
        let (scrubbed, removed) = scrub_legacy_dotenv(input.as_bytes()).unwrap();
        let scrubbed = String::from_utf8(scrubbed).unwrap();
        assert_eq!(removed, 2);
        assert!(scrubbed.contains("TUNNEL_TOKEN=tunnel"));
        assert!(scrubbed.contains("PANDORA_GITSYNC_TOKEN=git"));
        assert!(scrubbed.contains("DOODSTREAM_API_KEY=\n"));
        assert!(scrubbed.contains("LUMIERE_DRIVE_PROFILES=\n"));
    }

    #[test]
    fn positional_environment_scrub_preserves_operational_tokens() {
        let input = (0..=15)
            .map(|index| format!("value{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (scrubbed, removed) = scrub_legacy_environment(input.as_bytes()).unwrap();
        let scrubbed = String::from_utf8(scrubbed).unwrap();
        let lines = scrubbed.lines().collect::<Vec<_>>();
        assert_eq!(removed, LEGACY_ENV_LINE_INDICES.len());
        assert_eq!(lines[4], "value4");
        assert_eq!(lines[6], "value6");
        assert_eq!(lines[15], "value15");
        for index in LEGACY_ENV_LINE_INDICES {
            assert!(lines[*index].is_empty());
        }
    }

    #[test]
    fn guild_scrub_preserves_forgejo_and_new_server_policy() {
        let input = (0..=14)
            .map(|index| format!("value{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (scrubbed, removed) = scrub_guild_drive_fields(input.as_bytes()).unwrap();
        let scrubbed = String::from_utf8(scrubbed).unwrap();
        let lines = scrubbed.lines().collect::<Vec<_>>();
        assert_eq!(removed, LEGACY_GUILD_LINE_INDICES.len());
        assert_eq!(lines[3], "value3");
        assert_eq!(lines[13], "value13");
        assert_eq!(lines[14], "value14");
        for index in LEGACY_GUILD_LINE_INDICES {
            assert!(lines[*index].is_empty());
        }
    }

    #[test]
    fn process_environment_filter_is_scoped_to_secret_like_names() {
        assert!(sensitive_process_environment_name("PANDORA_GITSYNC_TOKEN"));
        assert!(sensitive_process_environment_name("TUNNEL_TOKEN"));
        assert!(sensitive_process_environment_name("CLOUDFLARE_API_KEY"));
        assert!(sensitive_process_environment_name("DOODSTREAM_API_KEY"));
        assert!(sensitive_process_environment_name("LUMIERE_BROKER_TOKEN"));
        assert!(!sensitive_process_environment_name("PATH"));
        assert!(!sensitive_process_environment_name("PANDORA_GITSYNC_REPO"));
    }

    #[test]
    fn backup_classifier_includes_credentials_but_not_arbitrary_work_files() {
        assert!(backup_path_matches(
            Path::new("DB/config/global/environment/env.pandora"),
            BackupScanKind::Database,
        ));
        assert!(backup_path_matches(
            Path::new("DB/config/1/meta.pandora"),
            BackupScanKind::Database,
        ));
        assert!(backup_path_matches(
            Path::new("DB/config/1/2/smartcode_drive/01.json"),
            BackupScanKind::Database,
        ));
        assert!(!backup_path_matches(
            Path::new("DB/work/1/work/output.mp4"),
            BackupScanKind::Database,
        ));
    }

    #[tokio::test]
    async fn backup_zip_contains_manifest_and_clears_source_buffers() {
        let mut backup = BackupCollection {
            entries: BTreeMap::from([(
                "files/env.pandora".to_string(),
                BackupEntry {
                    source_key: Some("env.pandora".to_string()),
                    bytes: SensitiveBytes::new(b"api_key=secret".to_vec()),
                },
            )]),
            process_environment_names: Vec::new(),
            notes: Vec::new(),
        };
        let manifest = KeyvaultManifest {
            schema: "pandora-keyvault/v1",
            created_at: 1,
            expires_at: 2,
            backup_id: "0".repeat(32),
            prepared_by_discord_user_id: "1".to_string(),
            purge_scope: "legacy-upload-credentials-only",
            planned_purge: PurgeSummary::default(),
            confirmation: KeyvaultConfirmation {
                instruction: "test",
                proof: "1".repeat(64),
            },
            included_files: vec!["files/env.pandora".to_string()],
            included_process_environment: Vec::new(),
            notes: Vec::new(),
        };
        let archive = build_backup_zip(&mut backup, &manifest).await.unwrap();
        assert!(archive.starts_with(b"PK"));
        assert!(
            backup.entries["files/env.pandora"]
                .bytes
                .as_slice()
                .iter()
                .all(|byte| *byte == 0)
        );
    }

    #[tokio::test]
    async fn purge_executor_atomically_replaces_and_removes_targets() {
        let root = std::env::temp_dir().join(format!(
            "pandora-keyvault-test-{}-{}",
            std::process::id(),
            random_hex(6).unwrap(),
        ));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let environment = root.join("env.pandora");
        let historical = root.join("gdrive_env.pandora");
        tokio::fs::write(&environment, b"legacy secret")
            .await
            .unwrap();
        tokio::fs::write(&historical, b"historical secret")
            .await
            .unwrap();
        let mut plan = PurgePlan {
            writes: vec![PlannedWrite {
                path: environment.clone(),
                original: SensitiveBytes::new(b"legacy secret".to_vec()),
                replacement: SensitiveBytes::new(b"scrubbed".to_vec()),
                values_removed: 1,
                kind: PurgeWriteKind::Environment,
            }],
            deletes: vec![PlannedDelete {
                path: historical.clone(),
                original: SensitiveBytes::new(b"historical secret".to_vec()),
            }],
            fingerprint: String::new(),
        };
        plan.fingerprint = purge_fingerprint(&plan.writes, &plan.deletes);
        assert!(execute_purge(&plan).await.is_empty());
        assert_eq!(tokio::fs::read(&environment).await.unwrap(), b"scrubbed");
        assert!(!historical.exists());
        tokio::fs::remove_dir_all(root).await.ok();
    }

    #[test]
    fn age_backup_round_trips_and_clears_plaintext() {
        let identity = age::x25519::Identity::generate();
        let recipient = identity.to_public();
        let mut plaintext = b"PK secret archive".to_vec();
        let encrypted = encrypt_for_recipient(&recipient, &mut plaintext).unwrap();
        assert!(plaintext.iter().all(|byte| *byte == 0));
        let decryptor = age::Decryptor::new(&encrypted[..]).unwrap();
        let mut reader = decryptor
            .decrypt(std::iter::once(&identity as &dyn age::Identity))
            .unwrap();
        let mut decrypted = Vec::new();
        reader.read_to_end(&mut decrypted).unwrap();
        assert_eq!(decrypted, b"PK secret archive");
    }

    #[test]
    fn proof_comparison_is_exact() {
        assert!(constant_time_eq(b"abcdef", b"abcdef"));
        assert!(!constant_time_eq(b"abcdef", b"abcdeg"));
        assert!(!constant_time_eq(b"abcdef", b"short"));
    }
}
