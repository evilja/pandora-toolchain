use crate::lib::mpeg::probe::ffprobe_video_height;
use crate::lumiere_broker::{
    DriveCandidate, DriveUploadResult, DriveUploadSpec, GLOBAL_DRIVE_PROFILE, LumiereClient,
    RemoteProvider, RemoteUploadSpec, UploadError, UploadProgress, content_type_for_path,
    guild_drive_profile,
};
use crate::pnworker::core::{CommData, SmartcodeDriveName};
use crate::pnworker::core::{Stage, WorkerMsg};
use crate::pnworker::messages::{
    BACKUPALL_PROG, JOB_CANCELLED, MessagePayload, UPLOAD_BACKUP_PROG, UPLOAD_DONE, UPLOAD_FAIL,
    UPLOAD_PROG, WORKER_ASSIGN,
};
use crate::pnworker::server_config::server_drive_only;
use crate::pnworker::util::string_byte_to_mb;
use crate::pnworker::util::{OUTPUT_RESOLUTION_FILE, WorkerNamePool, job_cancelled};
use crate::pnworker::worker_slots::upload_worker_slots;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::mpsc::{Receiver, Sender, channel, unbounded_channel};
use tokio::time::{Instant, sleep};

pub type UploadData = (
    PathBuf,
    String,
    bool,
    u64,
    u64,
    Option<u64>,
    Option<String>,
    Option<String>,
    Option<SmartcodeDriveName>,
);
pub type UploadAllData = (PathBuf, u64, Option<u64>);

pub async fn pn_uloadworker(mut rx: Receiver<WorkerMsg>, tx: Sender<CommData>, pulse: Sender<()>) {
    let mut pool = WorkerNamePool::new(upload_worker_slots().await);
    let mut next_slot_refresh = Instant::now() + Duration::from_secs(1);
    let (done_tx, mut done_rx) = channel::<String>(32);
    let mut pending: VecDeque<WorkerMsg> = VecDeque::new();

    loop {
        if Instant::now() >= next_slot_refresh {
            pool.set_names(upload_worker_slots().await);
            next_slot_refresh = Instant::now() + Duration::from_secs(1);
        }
        while let Ok(name) = done_rx.try_recv() {
            pool.release(&name);
        }
        while let Ok(msg) = rx.try_recv() {
            match msg {
                WorkerMsg::Upload(_) | WorkerMsg::UploadAll(_) => pending.push_back(msg),
                _ => {}
            }
        }
        while let Some(name) = pool.acquire() {
            let Some(msg) = pending.pop_front() else {
                pool.release(&name);
                break;
            };
            let tx2 = tx.clone();
            let done_tx2 = done_tx.clone();
            tokio::spawn(async move {
                run_lumiere_upload_job(msg, tx2, name.clone()).await;
                done_tx2.send(name).await.ok();
            });
        }
        sleep(Duration::from_millis(200)).await;
        pulse.try_send(()).ok();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LumiereHost {
    Drive,
    Doodstream,
    Lulustream,
    Voe,
    Abyss,
}

enum LumiereUploadEvent {
    Progress(LumiereHost, UploadProgress),
    Done(LumiereHost, String, Option<DriveUploadResult>),
    Failed(LumiereHost, String),
    Cancelled,
}

async fn run_lumiere_upload_job(msg: WorkerMsg, tx: Sender<CommData>, worker_name: String) {
    let assign_job_id = match &msg {
        WorkerMsg::Upload((_, _, _, job_id, _, _, _, _, _)) => Some(*job_id),
        WorkerMsg::UploadAll((_, job_id, _)) => Some(*job_id),
        _ => None,
    };
    if let Some(job_id) = assign_job_id {
        tx.try_send((
            job_id,
            MessagePayload::Progress(WORKER_ASSIGN, vec![format!("upl-{worker_name}")]),
            None,
        ))
        .ok();
    }
    let client = match LumiereClient::from_env() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("[lumiere] configuration error: {error}");
            if let Some(job_id) = assign_job_id {
                tx.send((
                    job_id,
                    MessagePayload::Static(UPLOAD_FAIL),
                    Some(Stage::Failed),
                ))
                .await
                .ok();
            }
            return;
        }
    };

    match msg {
        WorkerMsg::Upload((
            directory,
            out_name,
            release,
            job_id,
            _channel_id,
            server_id,
            gdrive_folder_global,
            gdrive_folder_local,
            smartcode_drive_name,
        )) => {
            run_lumiere_single_upload(
                client,
                directory,
                out_name,
                release,
                job_id,
                server_id,
                gdrive_folder_global,
                gdrive_folder_local,
                smartcode_drive_name,
                tx,
            )
            .await;
        }
        WorkerMsg::UploadAll((directory, job_id, server_id)) => {
            run_lumiere_upload_all(client, directory, job_id, server_id, tx).await;
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_lumiere_single_upload(
    client: LumiereClient,
    directory: PathBuf,
    out_name: String,
    release: bool,
    job_id: u64,
    server_id: Option<u64>,
    gdrive_folder_global: Option<String>,
    gdrive_folder_local: Option<String>,
    smartcode_drive_name: Option<SmartcodeDriveName>,
    tx: Sender<CommData>,
) {
    if job_cancelled(&directory) {
        tx.send((
            job_id,
            MessagePayload::Static(JOB_CANCELLED),
            Some(Stage::Cancelled),
        ))
        .await
        .ok();
        return;
    }
    let output_path = directory.join("work").join("output.mp4");
    let cancel_file = Some(directory.join("CANCEL"));
    let is_smartcode = gdrive_folder_local.is_some();
    let drive_only = release && server_drive_only(server_id).await;
    if drive_only {
        println!("[lumiere] job {job_id}: server policy restricts uploads to Google Drive");
    }
    let named_filename = match smartcode_drive_name.as_ref() {
        Some(name) => {
            Some(name.filename(&cached_output_resolution(&directory, &output_path).await))
        }
        None => None,
    };
    let candidates = lumiere_drive_candidates(
        server_id,
        is_smartcode,
        gdrive_folder_global,
        gdrive_folder_local,
        &out_name,
        named_filename.as_deref(),
    );
    let content_type = content_type_for_path(&output_path).to_string();
    let (event_tx, mut event_rx) = unbounded_channel();
    let mut tasks = Vec::new();

    tasks.extend(spawn_drive_upload(
        client.clone(),
        DriveUploadSpec {
            path: output_path.clone(),
            request_id: format!("pandora:{job_id}:drive"),
            candidates,
            content_type: content_type.clone(),
            cancel_file: cancel_file.clone(),
        },
        event_tx.clone(),
    ));
    if remote_uploads_enabled(release, drive_only) {
        for (host, provider) in [
            (LumiereHost::Doodstream, RemoteProvider::Doodstream),
            (LumiereHost::Lulustream, RemoteProvider::Lulustream),
            (LumiereHost::Voe, RemoteProvider::Voe),
        ] {
            tasks.extend(spawn_remote_upload(
                client.clone(),
                RemoteUploadSpec {
                    path: output_path.clone(),
                    request_id: format!(
                        "pandora:{job_id}:{}",
                        provider.label().to_ascii_lowercase()
                    ),
                    provider,
                    filename: out_name.clone(),
                    content_type: content_type.clone(),
                    cancel_file: cancel_file.clone(),
                },
                host,
                event_tx.clone(),
            ));
        }
        event_tx
            .send(LumiereUploadEvent::Failed(
                LumiereHost::Abyss,
                "Abyss remote upload API is not supported yet".to_string(),
            ))
            .ok();
    }
    drop(event_tx);

    let expected_hosts = expected_lumiere_hosts(release, drive_only);
    let mut completed = 0usize;
    let mut any_success = false;
    let mut gd_link = "Google Bekleniyor".to_string();
    let mut dood_link = if drive_only { String::new() } else { "Doodstream Bekleniyor".to_string() };
    let mut lulu_link = if drive_only { String::new() } else { "Lulustream Bekleniyor".to_string() };
    let mut voe_link = if drive_only { String::new() } else { "Voe Bekleniyor".to_string() };
    let mut abyss_link = if drive_only { String::new() } else { "Abyss Bekleniyor".to_string() };
    let mut done = [false; 5];
    let mut last_progress = [None; 5];
    let mut drive_meta: Option<(String, String, String, String)> = None;
    let track_smartcode = smartcode_drive_name.is_some();
    let mut cancelled = false;

    while let Some(event) = event_rx.recv().await {
        let mut emit_update = true;
        match event {
            LumiereUploadEvent::Progress(host, progress) => {
                let host_index = lumiere_host_index(host);
                if done[host_index] {
                    continue;
                }
                let now = Instant::now();
                emit_update = last_progress[host_index]
                    .map(|last| now.duration_since(last) >= Duration::from_secs(5))
                    .unwrap_or(true)
                    || progress.sent >= progress.total;
                if emit_update {
                    last_progress[host_index] = Some(now);
                }
                let text = upload_progress_text(
                    lumiere_host_label(host),
                    &progress.sent.to_string(),
                    &progress.total.to_string(),
                    "0",
                );
                set_lumiere_link(
                    host,
                    text,
                    &mut gd_link,
                    &mut dood_link,
                    &mut lulu_link,
                    &mut voe_link,
                    &mut abyss_link,
                );
            }
            LumiereUploadEvent::Done(host, url, result) => {
                if done[lumiere_host_index(host)] {
                    continue;
                }
                done[lumiere_host_index(host)] = true;
                completed += 1;
                any_success = true;
                set_lumiere_link(
                    host,
                    url,
                    &mut gd_link,
                    &mut dood_link,
                    &mut lulu_link,
                    &mut voe_link,
                    &mut abyss_link,
                );
                if let Some(result) = result
                    && track_smartcode
                    && result.profile.starts_with("guild:")
                    && result.root == "smartcode"
                {
                    drive_meta = Some((
                        result.file_id,
                        result.parent_id,
                        result.profile,
                        result.delete_token,
                    ));
                }
            }
            LumiereUploadEvent::Failed(host, error) => {
                if done[lumiere_host_index(host)] {
                    continue;
                }
                eprintln!("[lumiere] {}: {}", lumiere_host_label(host), error);
                done[lumiere_host_index(host)] = true;
                completed += 1;
                set_lumiere_link(
                    host,
                    format!("{} Başarısız", lumiere_host_label(host)),
                    &mut gd_link,
                    &mut dood_link,
                    &mut lulu_link,
                    &mut voe_link,
                    &mut abyss_link,
                );
            }
            LumiereUploadEvent::Cancelled => {
                cancelled = true;
                break;
            }
        }

        if completed >= expected_hosts {
            break;
        }
        if emit_update {
            tx.try_send(lumiere_upload_payload(
                job_id,
                release,
                UPLOAD_PROG,
                &gd_link,
                &dood_link,
                &lulu_link,
                &voe_link,
                &abyss_link,
                drive_meta.clone(),
                None,
            ))
            .ok();
        }
    }
    for task in tasks {
        task.abort();
    }
    if !cancelled && completed < expected_hosts {
        for host in [
            LumiereHost::Drive,
            LumiereHost::Doodstream,
            LumiereHost::Lulustream,
            LumiereHost::Voe,
            LumiereHost::Abyss,
        ]
        .into_iter()
        .take(expected_hosts)
        .filter(|host| !done[lumiere_host_index(*host)])
        {
            set_lumiere_link(
                host,
                format!("{} Başarısız", lumiere_host_label(host)),
                &mut gd_link,
                &mut dood_link,
                &mut lulu_link,
                &mut voe_link,
                &mut abyss_link,
            );
        }
    }

    if cancelled || job_cancelled(&directory) {
        if any_success {
            tx.send(lumiere_upload_payload(
                job_id,
                release,
                JOB_CANCELLED,
                &gd_link,
                &dood_link,
                &lulu_link,
                &voe_link,
                &abyss_link,
                drive_meta,
                Some(Stage::Cancelled),
            ))
            .await
            .ok();
        } else {
            tx.send((
                job_id,
                MessagePayload::Static(JOB_CANCELLED),
                Some(Stage::Cancelled),
            ))
            .await
            .ok();
        }
    } else if any_success {
        tx.send(lumiere_upload_payload(
            job_id,
            release,
            UPLOAD_DONE,
            &gd_link,
            &dood_link,
            &lulu_link,
            &voe_link,
            &abyss_link,
            drive_meta,
            Some(Stage::Uploaded),
        ))
        .await
        .ok();
    } else {
        tx.send((
            job_id,
            MessagePayload::Static(UPLOAD_FAIL),
            Some(Stage::Failed),
        ))
        .await
        .ok();
    }
    println!("[Pandora Lumiere] End of Session");
}

fn spawn_drive_upload(
    client: LumiereClient,
    spec: DriveUploadSpec,
    event_tx: tokio::sync::mpsc::UnboundedSender<LumiereUploadEvent>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let (progress_tx, mut progress_rx) = unbounded_channel();
    let progress_events = event_tx.clone();
    let progress_task = tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            progress_events
                .send(LumiereUploadEvent::Progress(LumiereHost::Drive, progress))
                .ok();
        }
    });
    let upload_task = tokio::spawn(async move {
        match client.upload_drive(spec, Some(progress_tx)).await {
            Ok(result) => {
                event_tx
                    .send(LumiereUploadEvent::Done(
                        LumiereHost::Drive,
                        result.url.clone(),
                        Some(result),
                    ))
                    .ok();
            }
            Err(error) if error.is_cancelled() => {
                event_tx.send(LumiereUploadEvent::Cancelled).ok();
            }
            Err(error) => {
                event_tx
                    .send(LumiereUploadEvent::Failed(
                        LumiereHost::Drive,
                        error.to_string(),
                    ))
                    .ok();
            }
        }
    });
    vec![progress_task, upload_task]
}

fn spawn_remote_upload(
    client: LumiereClient,
    spec: RemoteUploadSpec,
    host: LumiereHost,
    event_tx: tokio::sync::mpsc::UnboundedSender<LumiereUploadEvent>,
) -> Vec<tokio::task::JoinHandle<()>> {
    let (progress_tx, mut progress_rx) = unbounded_channel();
    let progress_events = event_tx.clone();
    let progress_task = tokio::spawn(async move {
        while let Some(progress) = progress_rx.recv().await {
            progress_events
                .send(LumiereUploadEvent::Progress(host, progress))
                .ok();
        }
    });
    let upload_task = tokio::spawn(async move {
        match client.upload_remote(spec, Some(progress_tx)).await {
            Ok(result) => {
                event_tx
                    .send(LumiereUploadEvent::Done(host, result.url, None))
                    .ok();
            }
            Err(error) if error.is_cancelled() => {
                event_tx.send(LumiereUploadEvent::Cancelled).ok();
            }
            Err(error) => {
                event_tx
                    .send(LumiereUploadEvent::Failed(host, error.to_string()))
                    .ok();
            }
        }
    });
    vec![progress_task, upload_task]
}

async fn run_lumiere_upload_all(
    client: LumiereClient,
    directory: PathBuf,
    job_id: u64,
    server_id: Option<u64>,
    tx: Sender<CommData>,
) {
    if job_cancelled(&directory) {
        tx.send((
            job_id,
            MessagePayload::Static(JOB_CANCELLED),
            Some(Stage::Cancelled),
        ))
        .await
        .ok();
        return;
    }
    let mut files = find_mkv_files(&directory.join("contents").join("torrent")).await;
    files.sort_by_key(|path| path.display().to_string());
    if files.is_empty() {
        tx.send((
            job_id,
            MessagePayload::Static(UPLOAD_FAIL),
            Some(Stage::Failed),
        ))
        .await
        .ok();
        return;
    }
    let mut rows = (0..files.len())
        .map(|index| format!("episode {:02}: Bekleniyor", index + 1))
        .collect::<Vec<_>>();
    let mut any_uploaded = false;
    let cancel_file = Some(directory.join("CANCEL"));
    tx.try_send((
        job_id,
        MessagePayload::Progress(BACKUPALL_PROG, vec![format_backupall_rows(&rows)]),
        None,
    ))
    .ok();

    for (index, file) in files.iter().enumerate() {
        if job_cancelled(&directory) {
            tx.send((
                job_id,
                MessagePayload::Static(JOB_CANCELLED),
                Some(Stage::Cancelled),
            ))
            .await
            .ok();
            return;
        }
        let label = format!("episode {:02}", index + 1);
        let filename = file
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("backup.mkv")
            .to_string();
        let candidates = lumiere_drive_candidates(server_id, false, None, None, &filename, None);
        let (progress_tx, mut progress_rx) = unbounded_channel();
        let upload = client.upload_drive(
            DriveUploadSpec {
                path: file.clone(),
                request_id: format!("pandora:{job_id}:drive:{index}"),
                candidates,
                content_type: content_type_for_path(file).to_string(),
                cancel_file: cancel_file.clone(),
            },
            Some(progress_tx),
        );
        tokio::pin!(upload);
        let mut last_progress = None;
        let result = loop {
            tokio::select! {
                result = &mut upload => break result,
                Some(progress) = progress_rx.recv() => {
                    let now = Instant::now();
                    let should_emit = last_progress
                        .map(|last| now.duration_since(last) >= Duration::from_secs(5))
                        .unwrap_or(true)
                        || progress.sent >= progress.total;
                    if should_emit {
                        last_progress = Some(now);
                        rows[index] = format!(
                            "{}: {}",
                            label,
                            upload_progress_text("", &progress.sent.to_string(), &progress.total.to_string(), "0"),
                        );
                        tx.try_send((
                            job_id,
                            MessagePayload::Progress(BACKUPALL_PROG, vec![format_backupall_rows(&rows)]),
                            None,
                        )).ok();
                    }
                }
            }
        };
        match result {
            Ok(result) => {
                rows[index] = format!("{}: {}", label, result.url);
                any_uploaded = true;
            }
            Err(UploadError::Cancelled) => {
                rows[index] = format!("{}: İptal Edildi", label);
                tx.send((
                    job_id,
                    MessagePayload::Progress(BACKUPALL_PROG, vec![format_backupall_rows(&rows)]),
                    Some(Stage::Cancelled),
                ))
                .await
                .ok();
                return;
            }
            Err(error) => {
                eprintln!("[lumiere] Drive backup {} failed: {}", index + 1, error);
                rows[index] = format!("{}: Başarısız", label);
            }
        }
        tx.try_send((
            job_id,
            MessagePayload::Progress(BACKUPALL_PROG, vec![format_backupall_rows(&rows)]),
            None,
        ))
        .ok();
    }

    tx.send((
        job_id,
        MessagePayload::Progress(BACKUPALL_PROG, vec![format_backupall_rows(&rows)]),
        Some(if any_uploaded {
            Stage::Uploaded
        } else {
            Stage::Failed
        }),
    ))
    .await
    .ok();
    println!("[Pandora Lumiere] End of BackupAll Session");
}

fn lumiere_drive_candidates(
    server_id: Option<u64>,
    is_smartcode: bool,
    global_folder: Option<String>,
    local_folder: Option<String>,
    filename: &str,
    named_filename: Option<&str>,
) -> Vec<DriveCandidate> {
    let mut candidates = Vec::new();
    if let Some(server_id) = server_id.filter(|id| local_drive_enabled(*id)) {
        let profile = guild_drive_profile(server_id);
        let folder_path =
            drive_folder_path(true, is_smartcode, Some(server_id), local_folder.clone());
        if is_smartcode {
            candidates.push(DriveCandidate {
                profile: profile.clone(),
                root: "smartcode".to_string(),
                folder_path: folder_path.clone(),
                filename: named_filename.unwrap_or(filename).to_string(),
            });
            candidates.push(DriveCandidate {
                profile,
                root: "anonymous".to_string(),
                folder_path,
                filename: filename.to_string(),
            });
        } else {
            candidates.push(DriveCandidate {
                profile: profile.clone(),
                root: "anonymous".to_string(),
                folder_path: folder_path.clone(),
                filename: filename.to_string(),
            });
            candidates.push(DriveCandidate {
                profile,
                root: "smartcode".to_string(),
                folder_path,
                filename: filename.to_string(),
            });
        }
    }
    candidates.push(DriveCandidate {
        profile: GLOBAL_DRIVE_PROFILE.to_string(),
        root: "default".to_string(),
        folder_path: drive_folder_path(false, is_smartcode, server_id, global_folder),
        filename: filename.to_string(),
    });
    candidates
}

fn local_drive_enabled(server_id: u64) -> bool {
    let path = PathBuf::from("DB")
        .join("config")
        .join(server_id.to_string())
        .join("meta.pandora");
    let value = std::fs::read_to_string(path)
        .ok()
        .and_then(|meta| meta.lines().nth(9).map(str::trim).map(str::to_string))
        .unwrap_or_else(|| "true".to_string());
    !matches!(value.as_str(), "false" | "0" | "disabled" | "off")
}

fn remote_uploads_enabled(release: bool, drive_only: bool) -> bool {
    release && !drive_only
}

fn expected_lumiere_hosts(release: bool, drive_only: bool) -> usize {
    if remote_uploads_enabled(release, drive_only) { 5 } else { 1 }
}

fn lumiere_host_index(host: LumiereHost) -> usize {
    match host {
        LumiereHost::Drive => 0,
        LumiereHost::Doodstream => 1,
        LumiereHost::Lulustream => 2,
        LumiereHost::Voe => 3,
        LumiereHost::Abyss => 4,
    }
}

fn lumiere_host_label(host: LumiereHost) -> &'static str {
    match host {
        LumiereHost::Drive => "Google",
        LumiereHost::Doodstream => "Doodstream",
        LumiereHost::Lulustream => "Lulustream",
        LumiereHost::Voe => "Voe",
        LumiereHost::Abyss => "Abyss",
    }
}

#[allow(clippy::too_many_arguments)]
fn set_lumiere_link(
    host: LumiereHost,
    value: String,
    drive: &mut String,
    doodstream: &mut String,
    lulustream: &mut String,
    voe: &mut String,
    abyss: &mut String,
) {
    match host {
        LumiereHost::Drive => *drive = value,
        LumiereHost::Doodstream => *doodstream = value,
        LumiereHost::Lulustream => *lulustream = value,
        LumiereHost::Voe => *voe = value,
        LumiereHost::Abyss => *abyss = value,
    }
}

#[allow(clippy::too_many_arguments)]
fn lumiere_upload_payload(
    job_id: u64,
    release: bool,
    message_id: &'static str,
    gd_link: &str,
    dood_link: &str,
    lulu_link: &str,
    voe_link: &str,
    abyss_link: &str,
    drive_meta: Option<(String, String, String, String)>,
    stage: Option<Stage>,
) -> CommData {
    let visible_meta = drive_meta
        .as_ref()
        .map(|(file_id, folder_id, _, _)| (file_id.clone(), folder_id.clone()));
    let mut payload = upload_payload(
        job_id,
        release,
        message_id,
        gd_link,
        dood_link,
        lulu_link,
        voe_link,
        abyss_link,
        visible_meta,
        stage,
    );
    if let Some((_, _, profile, delete_token)) = drive_meta
        && let MessagePayload::Progress(_, args) = &mut payload.1
    {
        args.push(profile);
        args.push(delete_token);
    }
    payload
}

async fn cached_output_resolution(
    directory: &std::path::Path,
    output_path: &std::path::Path,
) -> String {
    tokio::fs::read_to_string(directory.join("work").join(OUTPUT_RESOLUTION_FILE))
        .await
        .ok()
        .and_then(|value| valid_resolution_label(&value))
        .unwrap_or_else(|| resolution_label(&output_path.display().to_string()))
}

fn valid_resolution_label(value: &str) -> Option<String> {
    let value = value.trim();
    value
        .strip_suffix('p')
        .and_then(|height| height.parse::<u32>().ok())
        .filter(|height| *height > 0)
        .map(|height| format!("{}p", height))
}

fn resolution_label(path: &str) -> String {
    ffprobe_video_height(path)
        .map(|height| format!("{}p", height))
        .unwrap_or_else(|| "1080p".to_string())
}

fn drive_folder_path(
    local_drive: bool,
    is_smartcode: bool,
    server_id: Option<u64>,
    folder: Option<String>,
) -> String {
    let folder = folder
        .unwrap_or_default()
        .trim()
        .trim_matches('/')
        .to_string();
    if !local_drive {
        return match server_id {
            Some(id) if folder.is_empty() => id.to_string(),
            Some(id) => format!("{}/{}", id, folder),
            None => folder,
        };
    }
    if is_smartcode {
        return folder;
    }
    if folder.is_empty() {
        return "pntools".to_string();
    }
    if folder == "pntools" || folder.starts_with("pntools/") {
        folder
    } else {
        format!("pntools/{}", folder)
    }
}

fn is_video_ext(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "mkv" | "mp4" | "m4v" | "mov" | "avi" | "webm" | "ts" | "m2ts"
    )
}

async fn find_mkv_files(root: &std::path::Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut read = match tokio::fs::read_dir(&dir).await {
            Ok(r) => r,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = read.next_entry().await {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .and_then(|e| e.to_str())
                .map(is_video_ext)
                .unwrap_or(false)
            {
                result.push(path);
            }
        }
    }
    result
}

fn format_backupall_rows(rows: &[String]) -> String {
    let mut out = String::new();
    let mut hidden = 0usize;
    for row in rows {
        let next = if out.is_empty() {
            row.clone()
        } else {
            format!("\n{}", row)
        };
        if out.len() + next.len() > 1000 {
            hidden += 1;
        } else {
            out.push_str(&next);
        }
    }
    if hidden > 0 {
        out.push_str(&format!("\n...and {} more", hidden));
    }
    out
}

fn upload_progress_text(host: &str, sent: &str, total: &str, extensions: &str) -> String {
    let suffix = if extensions == "0" {
        String::new()
    } else {
        format!("+{}", extensions)
    };
    let progress = format!(
        "{}/{} MB{}",
        string_byte_to_mb(sent),
        string_byte_to_mb(total),
        suffix
    );
    if host.is_empty() {
        progress
    } else {
        format!("{} {}", host, progress)
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn smartcode_drive_name_formats_release_filename() {
        let name = SmartcodeDriveName::new("AkiraSubs/frieren", "Sousou no Frieren", 1);
        assert_eq!(
            name.filename("1080p"),
            "[AkiraSubs] Sousou no Frieren - Bölüm 01 [1080p].mp4",
        );
    }

    #[test]
    fn smartcode_candidates_prefer_guild_smartcode_then_global() {
        let candidates = lumiere_drive_candidates(
            Some(u64::MAX),
            true,
            Some("global/anime".to_string()),
            Some("Anime".to_string()),
            "release.mp4",
            Some("named.mp4"),
        );
        assert_eq!(candidates[0].profile, guild_drive_profile(u64::MAX));
        assert_eq!(candidates[0].root, "smartcode");
        assert_eq!(candidates[0].filename, "named.mp4");
        assert_eq!(candidates[1].root, "anonymous");
        assert_eq!(candidates.last().unwrap().profile, GLOBAL_DRIVE_PROFILE);
        assert_eq!(
            candidates.last().unwrap().folder_path,
            format!("{}/global/anime", u64::MAX)
        );
    }

    #[test]
    fn drive_folder_path_puts_smartcode_local_folder_at_root() {
        assert_eq!(
            drive_folder_path(true, true, Some(123), Some("Anime Name".to_string())),
            "Anime Name",
        );
    }

    #[test]
    fn drive_folder_path_keeps_pntools_for_non_smartcode_local_jobs() {
        assert_eq!(
            drive_folder_path(true, false, Some(123), Some("backup".to_string())),
            "pntools/backup",
        );
        assert_eq!(drive_folder_path(true, false, Some(123), None), "pntools");
    }

    #[test]
    fn drive_folder_path_keeps_global_server_prefix() {
        assert_eq!(
            drive_folder_path(false, true, Some(123), Some("Anime Name".to_string())),
            "123/Anime Name",
        );
    }

    #[test]
    fn cached_resolution_labels_are_validated() {
        assert_eq!(
            valid_resolution_label(" 1080p\n"),
            Some("1080p".to_string())
        );
        assert_eq!(valid_resolution_label("0p"), None);
        assert_eq!(valid_resolution_label("fullhd"), None);
    }

    #[test]
    fn drive_only_release_schedules_only_drive() {
        assert_eq!(expected_lumiere_hosts(true, true), 1);
        assert!(!remote_uploads_enabled(true, true));
        assert_eq!(expected_lumiere_hosts(true, false), 5);
        assert!(remote_uploads_enabled(true, false));
        assert_eq!(expected_lumiere_hosts(false, false), 1);
        assert!(!remote_uploads_enabled(false, false));
    }

    #[test]
    fn drive_only_release_preserves_release_payload_positions() {
        let payload = lumiere_upload_payload(
            1,
            true,
            UPLOAD_DONE,
            "https://drive.google.com/file/d/file/view",
            "",
            "",
            "",
            "",
            Some((
                "file".to_string(),
                "folder".to_string(),
                "guild:1".to_string(),
                "delete-token".to_string(),
            )),
            Some(Stage::Uploaded),
        );
        assert_eq!(crate::pnworker::messages::format_payload(&payload.1, "en"), "https://drive.google.com/file/d/file/view");
        let MessagePayload::Progress(_, args) = payload.1 else {
            panic!("expected progress payload");
        };
        assert_eq!(args.len(), 9);
        assert!(args[1..5].iter().all(|arg| arg.is_empty()));
        assert_eq!(args[5], "file");
        assert_eq!(args[6], "folder");
        assert_eq!(args[7], "guild:1");
        assert_eq!(args[8], "delete-token");
    }

    #[test]
    fn lumiere_payload_hides_drive_profile_after_display_hosts() {
        let payload = lumiere_upload_payload(
            1,
            true,
            UPLOAD_DONE,
            "drive",
            "dood",
            "lulu",
            "voe",
            "abyss",
            Some((
                "file".to_string(),
                "folder".to_string(),
                "guild:1".to_string(),
                "delete-token".to_string(),
            )),
            Some(Stage::Uploaded),
        );
        let rendered = crate::pnworker::messages::format_payload(&payload.1, "EN");
        assert!(!rendered.contains("guild:1"));
        assert!(!rendered.contains("delete-token"));
        let MessagePayload::Progress(_, args) = payload.1 else {
            panic!("expected progress payload");
        };
        assert_eq!(args.len(), 9);
        assert_eq!(args[7], "guild:1");
        assert_eq!(args[8], "delete-token");
    }
}

#[allow(clippy::too_many_arguments)]
fn upload_payload(
    job_id: u64,
    release: bool,
    message_id: &'static str,
    gd_link: &str,
    dood_link: &str,
    lulu_link: &str,
    voesx_link: &str,
    abyss_link: &str,
    drive_meta: Option<(String, String)>,
    stage: Option<Stage>,
) -> CommData {
    if release {
        let mut args = vec![
            gd_link.to_string(),
            dood_link.to_string(),
            lulu_link.to_string(),
            voesx_link.to_string(),
            abyss_link.to_string(),
        ];
        if let Some((file_id, folder_id)) = drive_meta {
            args.push(file_id);
            args.push(folder_id);
        }
        (job_id, MessagePayload::Progress(message_id, args), stage)
    } else {
        (
            job_id,
            MessagePayload::Progress(UPLOAD_BACKUP_PROG, vec![gd_link.to_string()]),
            stage,
        )
    }
}
