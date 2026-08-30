use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;

use crate::lib::joblog::find_job_logs;
use crate::pnworker::link::spec::LinkLogChunk;

// Tool logs are written on whichever machine ran the tool, and for a leased job that is not the one
// anybody reads them from. A node ships each log forward as it grows, so `/catlogs` and
// `GET /jobs/:id/logs*` answer for a remote job through `lib::joblog` with no new route and no new
// token tier — the same trick `publish.log` uses to reach those endpoints without being a job.
//
// Shipping incrementally rather than bundling at the end is the point: a log that only arrives when
// a job ends is no use for the case job logs exist for, which is a job that is stuck and has not
// ended at all.

// Per file, per renew. Encoder progress is throttled to roughly five seconds, so this is far more
// headroom than a log actually uses; a file that somehow outruns it simply catches up over the
// renews that follow rather than being dropped.
const MAX_CHUNK_BYTES: u64 = 256 * 1024;
// What one job's log may occupy on the coordinator. A node is authenticated, not trusted with the
// disk: past this the file stops growing and says so once.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

// Reads whatever each of a job's logs has gained since the offsets say it was last shipped.
// Non-destructive: the caller advances its offsets only once the renew carrying these succeeded,
// so a failed request costs a repeat rather than a hole.
pub async fn collect(
    job_id: u64,
    offsets: &HashMap<String, u64>,
) -> (Vec<LinkLogChunk>, HashMap<String, u64>) {
    let mut chunks = Vec::new();
    let mut advanced = offsets.clone();
    // Both locations are searched, because a job that has just ended has already had its logs moved
    // to `saved_data` by the time the client notices the terminal report.
    let Ok(Some(logs)) = find_job_logs(job_id).await else {
        return (chunks, advanced);
    };
    for file in &logs.files {
        let sent = offsets.get(&file.name).copied().unwrap_or(0);
        // A file shorter than what was shipped was replaced — a retry of the same job writing a
        // fresh log. Start it over rather than splicing new bytes onto old ones.
        let reset = file.bytes < sent;
        let from = if reset { 0 } else { sent };
        if file.bytes <= from {
            continue;
        }
        let take = (file.bytes - from).min(MAX_CHUNK_BYTES);
        let Some(text) = read_range(&file.path, from, take) else {
            continue;
        };
        let read = text.len() as u64;
        if read == 0 {
            continue;
        }
        advanced.insert(file.name.clone(), from + read);
        chunks.push(LinkLogChunk {
            name: file.name.clone(),
            offset: from,
            // Logs are read as text on the way out and written as text on the way in. ffmpeg
            // occasionally emits a byte that is not valid UTF-8, and a replacement character in a
            // transcript is a better outcome than refusing to ship the transcript.
            text,
            reset,
        });
    }
    (chunks, advanced)
}

fn read_range(path: &PathBuf, from: u64, take: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut buffer = vec![0u8; take as usize];
    let mut filled = 0usize;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(_) => break,
        }
    }
    buffer.truncate(filled);
    Some(String::from_utf8_lossy(&buffer).into_owned())
}

// Appends what a node shipped into the coordinator's own log directory for that job, which is where
// `lib::joblog` already looks.
pub fn apply(job_id: u64, chunks: &[LinkLogChunk]) {
    if chunks.is_empty() {
        return;
    }
    let directory = crate::lib::joblog::active_log_dir(job_id);
    if std::fs::create_dir_all(&directory).is_err() {
        return;
    }
    for chunk in chunks {
        if let Err(reason) = apply_chunk(&directory, chunk) {
            eprintln!("[link] job {job_id} log {:?}: {reason}", chunk.name);
        }
    }
}

fn apply_chunk(directory: &std::path::Path, chunk: &LinkLogChunk) -> Result<(), String> {
    // The name comes off the wire and is used as a path component. `lib::joblog` only ever lists
    // plain files in one directory, so anything that could address another one is not a name this
    // will write.
    if !is_plain_name(&chunk.name) {
        return Err("not a plain file name".to_string());
    }
    let path = directory.join(&chunk.name);
    let existing = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
    if chunk.reset && existing > 0 {
        std::fs::remove_file(&path).ok();
    }
    let current = if chunk.reset { 0 } else { existing };
    if current >= MAX_FILE_BYTES {
        return Ok(());
    }
    // Offsets make a repeat harmless: a chunk the node re-sent after a renew it never saw succeed
    // lands entirely behind what is already written, and is skipped rather than duplicated.
    if chunk.offset + (chunk.text.len() as u64) <= current {
        return Ok(());
    }
    let mut body = chunk.text.as_str();
    if chunk.offset < current {
        let skip = (current - chunk.offset) as usize;
        body = match body.get(skip..) {
            Some(rest) => rest,
            // The overlap fell inside a multi-byte character, which only happens if the two sides
            // disagree about the file; take the whole chunk rather than splitting one.
            None => body,
        };
    } else if chunk.offset > current && current > 0 {
        // Something did not arrive — a renew that failed and was never repeated. Say so in the
        // transcript rather than silently splicing two disjoint halves together.
        append(
            &path,
            &format!(
                "\n[link] … {} byte(s) of this log did not reach the coordinator\n",
                chunk.offset - current
            ),
        );
    }
    let remaining = MAX_FILE_BYTES.saturating_sub(current);
    if (body.len() as u64) > remaining {
        let cut = floor_char_boundary(body, remaining as usize);
        append(&path, &body[..cut]);
        append(&path, "\n[link] log truncated: this job has reached the per-file ceiling\n");
        return Ok(());
    }
    append(&path, body);
    Ok(())
}

fn append(path: &std::path::Path, text: &str) {
    use std::io::Write;
    let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    file.write_all(text.as_bytes()).ok();
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name != "."
        && name != ".."
        && !name.starts_with('.')
}

#[cfg(test)]
mod tests {
    use super::*;

    // The name is a path component taken straight off the wire.
    #[test]
    fn only_plain_file_names_are_written() {
        assert!(is_plain_name("PNmpeg_Encode42.log"));
        assert!(!is_plain_name("../../etc/passwd"));
        assert!(!is_plain_name("logs/inner.log"));
        assert!(!is_plain_name(".."));
        assert!(!is_plain_name(".hidden"));
        assert!(!is_plain_name(""));
    }

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "pandora-linklogs-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn chunk(name: &str, offset: u64, text: &str, reset: bool) -> LinkLogChunk {
        LinkLogChunk {
            name: name.to_string(),
            offset,
            text: text.to_string(),
            reset,
        }
    }

    #[test]
    fn consecutive_chunks_append_into_one_transcript() {
        let root = scratch("append");
        apply_chunk(&root, &chunk("a.log", 0, "first\n", false)).unwrap();
        apply_chunk(&root, &chunk("a.log", 6, "second\n", false)).unwrap();
        assert_eq!(read(&root, "a.log"), "first\nsecond\n");
        std::fs::remove_dir_all(&root).ok();
    }

    // A node that never saw a renew succeed re-sends the same bytes. Writing them twice would put
    // a duplicated block in the middle of a transcript somebody is trying to read.
    #[test]
    fn a_repeated_chunk_is_not_written_twice() {
        let root = scratch("repeat");
        apply_chunk(&root, &chunk("a.log", 0, "first\n", false)).unwrap();
        apply_chunk(&root, &chunk("a.log", 0, "first\n", false)).unwrap();
        assert_eq!(read(&root, "a.log"), "first\n");
        std::fs::remove_dir_all(&root).ok();
    }

    // A chunk that overlaps what is already written contributes only its new tail.
    #[test]
    fn an_overlapping_chunk_contributes_only_its_new_bytes() {
        let root = scratch("overlap");
        apply_chunk(&root, &chunk("a.log", 0, "abcdef", false)).unwrap();
        apply_chunk(&root, &chunk("a.log", 3, "defghi", false)).unwrap();
        assert_eq!(read(&root, "a.log"), "abcdefghi");
        std::fs::remove_dir_all(&root).ok();
    }

    // Two disjoint halves spliced together read as one continuous log and are a lie about what the
    // tool printed. The gap has to be visible.
    #[test]
    fn a_gap_is_recorded_rather_than_hidden() {
        let root = scratch("gap");
        apply_chunk(&root, &chunk("a.log", 0, "start\n", false)).unwrap();
        apply_chunk(&root, &chunk("a.log", 100, "later\n", false)).unwrap();
        let body = read(&root, "a.log");
        assert!(body.contains("did not reach the coordinator"), "{body}");
        assert!(body.ends_with("later\n"), "{body}");
        std::fs::remove_dir_all(&root).ok();
    }

    // A retried job writes a fresh log from zero; splicing it onto the previous attempt's would
    // produce a transcript of neither run.
    #[test]
    fn a_reset_replaces_the_previous_transcript() {
        let root = scratch("reset");
        apply_chunk(&root, &chunk("a.log", 0, "old run\n", false)).unwrap();
        apply_chunk(&root, &chunk("a.log", 0, "new run\n", true)).unwrap();
        assert_eq!(read(&root, "a.log"), "new run\n");
        std::fs::remove_dir_all(&root).ok();
    }

    // The name is the one field that reaches the filesystem, so a refusal has to be a refusal and
    // not merely a name that happens to resolve somewhere harmless.
    #[test]
    fn a_traversing_name_writes_nothing() {
        let root = scratch("traverse");
        assert!(apply_chunk(&root, &chunk("../escaped.log", 0, "nope", false)).is_err());
        assert!(!root.parent().unwrap().join("escaped.log").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    fn read(root: &PathBuf, name: &str) -> String {
        std::fs::read_to_string(root.join(name)).unwrap()
    }
}
