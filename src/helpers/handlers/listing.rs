use super::*;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc::Sender;
use tokio::sync::oneshot;

// What a torrent turned out to hold, for a command that has to know before it can act and has no
// worker job of its own to do the asking: `/source` writes a file and queues nothing, and a
// subtitle archive handed to `/encode do` needs the file list to pair against before there is a
// batch to queue. Both used to make the user run a `/probe` command first and copy its job id across.
pub struct SourceListing {
    // The probe that produced the list. It stays in the queue at `Probed` for its usual window, so
    // a batch or a `Pancode` naming it adopts the `.torrent` it fetched rather than fetching again.
    pub probe_job_id: u64,
    // `(file index, label)` in the episode-sorted order the list is rendered in.
    pub rows: Vec<(u64, String)>,
    // The rendered rows themselves, which is what the page buttons re-read and swap in.
    pub text: String,
}

// How long a probe may go without so much as a row in the job DB. A job the queue refused — too
// long, or a coordinator that cannot place it — is never inserted, so silence this long is that.
const LISTING_ADMISSION_TIMEOUT: Duration = Duration::from_secs(30);
// How long the listing may take end to end. Generous because an orchestrator holds a probe for a
// node, and a magnet has to find its metadata before it has a file list to give.
const LISTING_TIMEOUT: Duration = Duration::from_secs(300);

// Whether a link is something a listing can be asked of. A Drive or direct link is one file by
// construction, and the prober refuses them for that reason.
pub fn is_listable_source(link: &str) -> bool {
    matches!(nyaaise(link), TorrentType::Link(_) | TorrentType::Magnet(_)) && !link.trim().is_empty()
}

// Runs an ordinary probe with no message of its own and waits for its answer in the job DB. The
// probe goes through the same queue every other job does — preview pool, a node's lease, the lot — so
// nothing about listing is reimplemented here; this only submits it and reads what it persisted.
pub async fn list_source(
    tx: &Sender<JobClass>,
    command: &serenity::all::CommandInteraction,
    link: &str,
) -> Result<SourceListing, String> {
    let job = Job::new_api(
        command.user.id.get(),
        command.channel_id.get(),
        JobType::Probe,
        nyaaise(link),
        Vec::new(),
        read_lang(command.guild_id),
        command.guild_id.map(|guild| guild.get()),
    );
    let probe_job_id = job.job_id;
    tx.send(JobClass::Job(job))
        .await
        .map_err(|_| "the worker queue is not running".to_string())?;

    let db = JobDb::new()
        .await
        .map_err(|e| format!("failed to open job DB: {}", e))?;
    let started = tokio::time::Instant::now();
    loop {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let row = db
            .get_job(probe_job_id)
            .await
            .map_err(|e| format!("failed to read the listing: {}", e))?;
        let Some(row) = row else {
            if started.elapsed() > LISTING_ADMISSION_TIMEOUT {
                return Err("the source could not be listed: the queue did not accept the request (it may be full)".to_string());
            }
            continue;
        };
        match listing_state(row.stage) {
            ListingState::Ready => {
                let rows = probe_rows(row.progress.as_deref());
                if rows.is_empty() {
                    return Err("the source lists no video files".to_string());
                }
                let text = probe_list_text(row.progress.as_deref()).unwrap_or_else(|| {
                    rows.iter()
                        .map(|(index, label)| format!("`{}` — {}", index, label))
                        .collect::<Vec<_>>()
                        .join("\n")
                });
                return Ok(SourceListing { probe_job_id, rows, text });
            }
            ListingState::Dead => {
                return Err("the source could not be inspected".to_string());
            }
            ListingState::Running => {}
        }
        if started.elapsed() > LISTING_TIMEOUT {
            return Err("the source took too long to list".to_string());
        }
    }
}

enum ListingState {
    Running,
    Ready,
    Dead,
}

// The stage column is the integer `lib::db::core::stage_to_int` writes.
fn listing_state(stage: i64) -> ListingState {
    match stage {
        21 => ListingState::Ready,
        7 | 8 | 9 => ListingState::Dead,
        _ => ListingState::Running,
    }
}

fn probe_list_text(progress: Option<&str>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(progress?).ok()?;
    value
        .get("file_text")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

// An answer somebody is waited on for, by the command that asked rather than by a queued job. The
// worker's own picks (`JobClass::Pick`) cover every command that *is* a job; this is for the ones
// that are not one yet — `/source`, which never queues anything, and a batch that has to know
// which files it is for before it exists. Keyed by who was asked and where, exactly as the worker
// matches its own.
struct PendingAnswer {
    // What counts as an answer. Everything else the person types is conversation and is left
    // alone, so a question being open never swallows the channel.
    accepts: Box<dyn Fn(&str) -> bool + Send>,
    answer: oneshot::Sender<String>,
}

fn pending_answers() -> &'static Mutex<HashMap<(u64, u64), PendingAnswer>> {
    static PENDING: OnceLock<Mutex<HashMap<(u64, u64), PendingAnswer>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

// Offers a chat message to whatever command is waiting on that person in that channel. Returns
// whether it was taken, so the caller knows not to pass it on to the queue.
pub fn take_pending_answer(author: u64, channel_id: u64, text: &str) -> bool {
    let mut pending = pending_answers().lock().unwrap();
    let key = (author, channel_id);
    if !pending.get(&key).is_some_and(|waiting| (waiting.accepts)(text)) {
        return false;
    }
    match pending.remove(&key) {
        Some(waiting) => waiting.answer.send(text.to_string()).is_ok(),
        None => false,
    }
}

// Waits for the author to send something `accepts` recognises. A second question to the same
// person in the same channel replaces the first, whose wait then ends as unanswered.
pub async fn await_pending_answer(
    author: u64,
    channel_id: u64,
    accepts: impl Fn(&str) -> bool + Send + 'static,
    timeout: Duration,
) -> Option<String> {
    let (answer, receiver) = oneshot::channel();
    pending_answers().lock().unwrap().insert(
        (author, channel_id),
        PendingAnswer { accepts: Box::new(accepts), answer },
    );
    match tokio::time::timeout(timeout, receiver).await {
        Ok(answer) => answer.ok(),
        // Only a timeout still owns the entry. A wait whose sender was dropped has already been
        // replaced, and removing the key then would cancel the question that replaced it.
        Err(_) => {
            pending_answers().lock().unwrap().remove(&(author, channel_id));
            None
        }
    }
}

// The single-index form: one of the offered file indices, as a bare number.
pub async fn await_pending_pick(
    author: u64,
    channel_id: u64,
    offered: Vec<u64>,
    timeout: Duration,
) -> Option<u64> {
    let accepts = move |text: &str| {
        text.trim()
            .parse::<u64>()
            .is_ok_and(|index| offered.contains(&index))
    };
    await_pending_answer(author, channel_id, accepts, timeout)
        .await
        .and_then(|text| text.trim().parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_pending_pick_takes_only_an_offered_index_from_its_author() {
        let wait = tokio::spawn(await_pending_pick(1, 100, vec![0, 4], Duration::from_secs(5)));
        // Let the wait register before anything is offered to it.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!take_pending_answer(2, 100, "4"));
        assert!(!take_pending_answer(1, 101, "4"));
        assert!(!take_pending_answer(1, 100, "3"));
        // Anything that is not an answer is conversation, and the question stays open.
        assert!(!take_pending_answer(1, 100, "which one was it again"));
        assert!(take_pending_answer(1, 100, "4"));
        assert_eq!(wait.await.unwrap(), Some(4));
        // Answered once; the same number again is just a number.
        assert!(!take_pending_answer(1, 100, "4"));
    }

    #[tokio::test]
    async fn an_unanswered_pick_stops_listening() {
        assert_eq!(
            await_pending_pick(7, 700, vec![1], Duration::from_millis(30)).await,
            None
        );
        assert!(!take_pending_answer(7, 700, "1"));
    }

    #[test]
    fn the_listing_reads_the_stage_the_job_db_writes() {
        use pandora_toolchain::lib::db::core::stage_to_int;
        use pandora_toolchain::pnworker::core::Stage;
        assert!(matches!(listing_state(stage_to_int(Stage::Probed)), ListingState::Ready));
        for stage in [Stage::Failed, Stage::Declined, Stage::Cancelled] {
            assert!(matches!(listing_state(stage_to_int(stage)), ListingState::Dead));
        }
        for stage in [Stage::Queued, Stage::Probing] {
            assert!(matches!(listing_state(stage_to_int(stage)), ListingState::Running));
        }
    }

    #[test]
    fn only_torrents_and_magnets_can_be_listed() {
        assert!(is_listable_source("https://nyaa.si/view/1234567"));
        assert!(is_listable_source("magnet:?xt=urn:btih:abcdef"));
        assert!(!is_listable_source("https://drive.google.com/file/d/abc/view"));
        assert!(!is_listable_source(""));
    }
}
