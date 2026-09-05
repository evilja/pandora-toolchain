# WORKER.md

Worker runtime, tool orchestration, torrent routing, and cache/duplicate behavior.

## Patterns

- **Worker → tool wiring pattern**: declare a `CliParam` slice in `pnworker/tools.rs`, then call `run_tool(&path, SPEC, &HashMap<&str, PathValue>, job_id, &mut proto, |data| {...})`. The closure inspects `data.get(0).and_then(|v| v.parse::<u16>())` — opcodes are `0` progress, `1` success, `2` fail, `3` cancel, `4` is custom (used by probe rows, downloader file selection, and pnass line-length warnings — see [TOOLS.md](TOOLS.md)), `5` is pnp2p duplicate torrent (`["5", "DUPLICATE_TORRENT", save_path]`), and `6` is pnp2p per-file completion inside a multi-file selection (`["6", [index, name]]`). Return `Some(ToolResult::...)` to break.
- **A spec's `Path` keys must all be in the map**: `util::tool_args` builds the argv and nothing at compile time connects a `CliParam::Path("KEY")` in `tools.rs` to the `HashMap` its caller passes, so a key added to one and not the other is only visible at runtime. A missing key returns `ToolResult::Fail` with the key's name on stderr, which fails the job immediately rather than panicking the worker task and letting the stall watchdog find it twenty minutes later. `CliParam::RepeatedPath` is the exception: a missing key emits no arguments and no error. The three pnp2p call sites in `downloadworker.rs` share one `p2p_params` builder for exactly this reason, and its tests walk each spec against it.
- **Worker tool negkeys**: worker-run tools pass stable protocol negkeys by worker slot, not by tool/job id: `pn-download-<name>`, `pn-upload-<name>`, `pn-encode-main`, and `pn-probe-<name>`. Keep those aligned with `Job.worker` labels because `/workers` uses the same names.
- **Configurable download/preview/upload slots**: `DB/config/global/environment/workers.toml` stores `download = [...]`, `probe = [...]`, and `upload = [...]` (the `probe` key is retained for config compatibility); legacy two-key files receive the default `hoshi`/`kumo` preview slots. `/touchworker`, `/lsworker`, and `/rmworker` manage all three pools. Their orchestrators refresh the config while running; removed active slots finish their current job and then disappear from the available pool.
- **`/workers` layout**: renders a Discord embed diagram with exactly three inline fields: `download`, `core`, and `upload`. Download slots occupy the download column, probe slots plus `enc-main` occupy the core column, and upload slots occupy the upload column. Odd/even slot counts are ordered around the center with blank-line padding inside the column values, not placeholder fields. Column cells show only active/idle markers and worker names; the non-inline `active` field uses `<organisation> <stage> <job type> <worker>`, with organisation details derived from `SmartcodeDriveName` or channel `meta.toml` repo URL and `anonymous` for jobs without one.
- **Protocol output from binaries**: `pn_emit!(protocol = proto, negkey = &neg, schema = [leaf, leaf, ...], data = [..., ..., ...])`. The `data` macro splits on top-level commas only — `if/else` and other multi-token expressions inside the array confuse it; bind to a `let` first and use the binding.
- **Tool progress throttling**: tools that throttle protocol progress to roughly 5s (`pnmpeg` encode progress and `pncurl` upload progress) should emit the **first** progress payload immediately, then start the 5s timer from that first emitted payload. Do not initialize throttle timers to process start time if that would hide the initial emit.
- **CommData**: workers send `(u64, MessagePayload, Option<Stage>)` upstream — see [LOCALIZATION.md](LOCALIZATION.md) for the message types. Stage drives the `pn_worker` state machine in `pnworker/core.rs`. `MessagePayload::Progress(WORKER_ASSIGN, vec![worker_name])` is internal: `core.rs` updates `job.worker` and does not render it as progress text. `/workers` builds its Discord embed from this live in-memory queue state.
- **Parallel worker orchestrators**: `pn_dloadworker`, `pn_probeworker`, and `pn_uloadworker` are single shrine layers that spawn one per-job task for each configured slot. Each spawned task owns its own `Protocol`. Names render as `dwl-<name>`, `prw-<name>`, and `upl-<name>` and are released through a done channel after the task exits. Probe, subtitle screenshot preview, and Discord Pandora Studio MP4 preview jobs share the preview pool. Pending/cache states include `dwl-pending`, `prw-pending`, `upl-pending`, and `dwl-cache`; `enc-main` remains fixed. The encoder layer waits directly on its channel with a heartbeat timeout rather than polling every five seconds, so download→encode status changes are dispatched immediately.
- **Subtitle attachments are normalised at queue time**: `prepare_queued_job` runs a non-empty `job.attachment` through `lib::subs::ensure_ass_bytes` before writing `contents/subtitle.ass`, so libass only ever sees ASS. The attachment reaches the worker as bare bytes (no filename survives the Discord/API submit), so the format is decided by sniffing content; anything ffmpeg can demux as text is converted in place — and rescaled off ffmpeg's 384x288 canvas onto 1920x1080, without which a server watermark could never be merged into it — while image-based or non-UTF-8 input declines the job with that specific reason. `prepare_queued_job` returns `Result<(), String>` for exactly this reason — the caller passes the reason straight to `decline_job_setup` instead of the generic "could not prepare the work directory". Conversion happens **before** `encode_forward_key` is computed, so forwarding still dedupes two identical uploads and never shares an encode between different sources.
- **Pandora Studio rendering**: Discord handlers snapshot a Studio manifest and hard-linked/copied assets into `DB/work/<job>/contents/studio` before queue submission. Discord `StudioPreview` runs `pnmpeg --studio` on a `prw-*` slot and attaches `work/studio-preview.mp4`; full `Studio` renders run on `enc-main`, write `work/output.mp4`, then enter the ordinary multihost upload path. The Studio webpage does not submit preview jobs: it streams range-addressable source media and applies insert/override/duck audio with Web Audio in the browser. Encode-kind final sources stream-copy video, while Backup-kind final sources use the snapshotted server preset. Server jobs honor the normal `CANCEL` sentinel and worker non-resume policy. Studio metadata remains available independently until its 24-hour active or 30-minute unowned TTL.
- **Lumiere uploads**: `pn_uloadworker` performs uploads in-process through `src/lumiere-broker` rather than sending provider credentials to `pncurl`. Google bytes stream directly from the VDS through a broker-issued resumable session; Byse/LuluStream/Voe pull from separate memory-only capability URLs served by the existing Axum API. DoodStream and Abyss were removed in August 2026 — DoodStream after a second player-domain rotation, Abyss because its only documented upload is a push to `up.abyss.to/<api_key>`, which puts the credential back on the VDS and therefore cannot be brokered. When server metadata line 14 is enabled through `/edit drive_only:true`, a release schedules only the Drive task and creates no external streaming-host transfer capability. Metadata line 17, managed by `/edit hls:true|false`, makes Lumiere Files HLS the exclusive release output: the upload worker schedules none of Drive/Byse/LuluStream/Voe, adopts `work/hls` whole (one rename) when the encode already muxed the layout there, and otherwise ffmpeg stream-copies the finished MP4 into a single-variant master playlist `<name>.m3u8` and media playlist `<name>_variant.m3u8`. H.264 uses four-second-target MPEG-TS chunks named `chunk/p<n>-<name>.ts`; AV1 uses fMP4/CMAF with `chunk/init-<name>.mp4` and `p<n>-<name>.m4s`, and the master advertises the probed `av01…` and AAC RFC 6381 codec strings. The four seconds are a target, not a fixed length, because stream copy can only cut on a keyframe. `<name>` is metadata line 18's template — `%uuid%`, `%random%`, and `%res%`, defaulting to `%uuid%_%random%_%res%` — rendered per output: the UUID and the six random hex characters are fresh each time, and the height comes from ffprobe capped by the encode preset's scale, falling back to `1080p`. The encode worker passes the server's template to `pnmpeg --hls-name` so a job that muxes its own layout names it exactly as the publisher would have; the publisher renders it itself when it is the one remuxing. An adopted layout is read back off the media playlist's own filename rather than re-derived from the template, so a template edited between encode and upload cannot orphan a release. The broker serves the layout through `/lumiere/v1/hls/<capability>/` with playlist, TS, fMP4-fragment, and init-segment content types, then removes it after 12 hours. The master capability URL is the sole public link in the completed job. The capability and expiry survive restarts; cancelled jobs revoke an HLS output they already created. HLS reuses the retired public payload slot, so the positional protocol is `[drive, byse, lulustream, voe, hls]` and private Drive metadata remains at index 5 and later. Active upload tasks do not change when either policy is edited.
- **Upload logging**: every stage of an upload prints to `pndc`'s stdout/stderr as `[lumiere] <hh:mm:ss>Z <scope> | <message>`, where scope is the Drive/remote request id (`pandora:<job>:<host>`), `xfer <token prefix>` for a capability, or `broker` for Worker calls. The job loop adds `[lumiere] job <id>:` lines, including a 60s heartbeat naming the hosts that have not reported, since a hung host emits no events of its own. Remote hosts log every provider state change, a 60s heartbeat with bytes served versus provider-reported progress, and an explicit warning when a provider has not fetched its capability URL within 120s. `serve_transfer` logs each provider fetch with its IP/user agent, every 404/416 with the reason, and whether the stream finished or the provider disconnected early. Remote polls send `source_drained` once the whole file has been served so the Worker can confirm completion through the provider's `file/info`, and `lumiere_remote_stall_secs` (default 900, `0` disables) fails a host that reports no state, byte, or percentage movement for that long instead of pinning the job until the transfer TTL. See [LUMIERE_BROKER.md](LUMIERE_BROKER.md) for reading these on a production host.
- **Drive deletion capabilities**: every completed Encode/Pancode/Keycode/Studio Drive upload — including a DEV/Dummy encode that does not schedule streaming hosts — carries hidden Drive file/folder/profile/root metadata plus its per-file deletion capability after five public host slots (non-release payloads pad the unused slots). Rendering and `progress.rs` expose none of the capability/root data. `drive_cleanup.rs` stores one mode-`0600` copy per Encode/Pancode/Keycode/Studio job under `DB/config/global/environment/drive_deletions/<job>.json`, including forwarded jobs that share the same Drive file. A job-author or Witch 💔 request verifies the archived job and calls Lumiere; success consumes every state file for that shared upload, removes Drive fields from those jobs' links/progress/pending AnimeciX state, and leaves public streaming hosts intact. Named local Smartcode uploads additionally keep the established `DB/config/<server>/<channel>/smartcode_drive/<episode>.json` pointer; replacement or 💔 consumes both records. Pre-feature uploads and legacy Smartcode state without a capability fail closed to manual cleanup.
- **Shrine supervisor (`pnworker/heartbeat/`)**: `TypedShrine` reboots a layer when its task finishes (panicked or returned) or when it goes **>160s without a heartbeat**. A heartbeat is either an explicit `pulse.try_send(())` or *any* upstream `CommData` (`TypedLayer::try_recv` refreshes `last_heartbeat`). This 160s watchdog is the whole point of Shrine — it exists to catch a wedged encode/probe, not just a crashed one.
- **Heartbeat behavior**: `enc-main` runs synchronously and relies on its `CommData` progress stream while busy. Download, probe/preview, and upload layers spawn per-job tasks and pulse every 200ms, so their shrine liveness is independent of individual job progress. VerySlow's parallel chunk driver aggregates completed frames across workers and emits the same throttled pnmpeg progress protocol, so the shrine and `ENCODE_STALL_TIMEOUT` still see one advancing `enc-main` job.
- **Reboot is intentionally non-resuming**: `reboot()` brings the layer back with an empty queue and does **not** replay the last message (the replay block is left commented out on purpose). The job that was mid-flight is dropped/orphaned by design. Encode jobs are the exception: `check_encode_reboot_epoch` → `reset_encode_dispatches_after_reboot` clears `encode_dispatched` so the queue re-sends them to the new layer. That reset skips any job whose `encode_dispatch_epoch` is already the current one — `shrine.send` reboots an expired layer *before* sending, so without the guard the job dispatched microseconds earlier would be cleared and sent again, putting two encoders on one work directory. Phantom-active rows are reconciled by `fail_stale_active()` at the next `pn_worker` startup, not live.
- **`run_tool` sets `kill_on_drop(true)`**: aborting a wedged layer drops the future wherever it is parked, and without this the tool process keeps running with nobody reading its stdout. Since a stall reboots the same layer every 160s, each cycle would otherwise leave another encoder behind competing for the machine.
- **`Job`** carries a `lang: String` field. It's set at job creation in `pndc.rs` by reading `DB/config/<guild_id>/meta.pandora` line 0 and flowing through every worker call. The `lang` is what `create_job_embed` and `format_payload` use to look up strings.
- **`Job.frontend: Frontend`** (in `pnworker/frontend.rs`) is the originating surface — `Discord { ctx, msg }`, `Web`, or `None` — replacing the old raw serenity context tuple. All status output goes through it (`update` / `set_text` / `mark_failed` / `set_presence` / `notify_recompiling`); `Web`/`None` variants are no-ops, so the worker pipeline is frontend-agnostic. Because a method borrows `frontend` mutably while reading the rest of the job, `core.rs`'s `render()` helper does `std::mem::replace(&mut job.frontend, Frontend::None)`, calls `update`, then restores it. Discord jobs use the normal constructor; API jobs use `Job::new_api(...)`, which sets `Frontend::Web`, `response_id = 0`, and a nanosecond `job_id`. `Job` also carries `server_id: Option<u64>` (originating guild, used by the upload workers), `worker: String` (shown in embeds instead of owner), and `duplicate_source: Option<PathBuf>` (used while waiting on duplicate/cached inputs).

## Subtitle extraction

`/subs` is a `JobType::Subs` job that downloads like any other and then runs `pnmpeg --extractsubs` on the **preview pool** (`prw-*`), because it is the same shape of work as a preview: one downloaded input, one tool invocation, files attached back to the job message. It carries no subtitle attachment and no preset, so `queue_subs_job` calls `prepare_queued_job(.., write_subtitle = false)` and goes straight to the downloader; a `probe_job_id` copies the probe's `fetch.torrent` and selects that file index, exactly as `/encode pan` does.

`run_subs_job` collects one opcode `4` row per track (see [TOOLS.md](TOOLS.md#pnmpeg---extractsubs)) into extracted and skipped lists, then decides what to attach: a single track travels as itself, several are bundled into `work/subs-<job id>.zip` so the message carries one attachment rather than a column of them. A failed bundle falls back to attaching the first track instead of failing the job. Zero extracted tracks is `SUBS_NONE` carrying the per-track skip reasons — that is the normal answer for a release whose only subtitles are PGS, and it is a terminal state rather than an error, because nothing went wrong.

`Frontend::update` treats `SUBS_DONE` like the preview payloads: `is_attachment_done` routes it to `preview_done_edit`, which attaches `args[1]` and falls back to `SUBS_ATTACHMENT_MISSING` on the embed when Discord rejects the file.

## Batch encodes

`/encode batch` produces a `JobType::Batch` **parent** plus one `JobType::Pancode` **child** per episode. The parent carries `Job.batch: Option<BatchRequest>` (`pnworker/batch.rs`); every child carries `Job.batch_parent: Option<u64>`. Exactly one of the two is ever set, and `do_job_progression_things` skips parents outright — a parent never encodes anything itself.

- **One download, many files.** The info-hash lock in `lib::p2p` admits a single downloader per torrent, so a batch cannot be N parallel `pnp2p` calls. The parent dispatches one `WorkerMsg::Download` whose index list holds every selected file (`DownloadData`'s fourth field is now a `Vec<u64>`: empty = whole torrent, one entry = `/encode pan`, many = batch → `pnp2p --selects`). It is dispatched with `preserve_all`, so nothing is renamed to `input.mkv` in the parent's directory.
- **Finished files leave early.** `TorrentClient` tracks the piece span of each selected file and emits `DownloadEvent::FileComplete` when the last piece covering it is written, flushing that file first (`Storage::flush_file`). `pnp2p` re-emits it as opcode `6` (`["6", [index, name]]`), the download worker forwards it as the internal `TORRENT_FILE_DONE` payload, and `core.rs::spawn_batch_child` turns it into a queued encode. `PieceScheduler::claim` already hands out pieces in ascending index order and torrent files are laid out contiguously, so the file closest to completion is the one that completes next. The client separately tracks each file's contiguous verified prefix: storage preallocates the final file length, so downstream streaming readers use `DownloadEvent::FilePrefix`/`work/download.prefix` rather than mistaking unwritten sparse bytes for downloaded media. Direct and Drive downloads publish the same sidecar from their sequential byte count.
- **Episodes may be leased one index at a time.** `do_batch_lease_things` offers unclaimed entries
  to Pandora Mini nodes before the parent has downloaded them, as a Pancode carrying the parent's
  torrent and that file index. A leased episode is fetched twice — the parent's selection cannot be
  narrowed once its session is open — which trades coordinator bandwidth for cluster encoders. See
  [LINK.md](LINK.md#batch-split).
- **The child is born downloaded.** `build_batch_child` clones the parent (preset, watermark, server effects, Drive folders, language all carry over), hard-links the finished file to its own `contents/torrent/input.mkv` — falling back to a copy — writes the paired subtitle, and enters the queue at `Stage::Downloaded`. Children get `probe_job_id: None` so the first one to finish does not archive the probe job out from under its siblings. They are pushed straight onto the queue, and the `queue.len() > 4` submission gate counts only non-child jobs, so one batch cannot decline everyone else's work.
- **Deprioritization (`batch_child_may_dispatch`).** A batch child is held back when another batch child is already dispatched or encoding, and it yields to ordinary encodes until **two** of them have gone ahead of it — `pn_worker` keeps the counter as `encodes_since_batch`. "Ordinary encode waiting" means one that could dispatch *right now* (`Stage::Downloaded`, or a queued `Keycode`), never one still downloading, so the encoder is never left idle waiting for a rival. With nothing else queued the batch runs back to back.
- **Reporting.** With `api_public_url` set, the batch speaks through its own message: the normal stage pipeline plus `episodes` and an `output` field linking `/batch/<token>` (see [API.md](API.md)), and every child runs on `Frontend::None`. Without it there is no page to link, so each child gets its own Discord message created from the parent's context (`Frontend::spawn_child_message`). The parent re-renders on child `Encoding`/`Uploading`/terminal transitions only, not on encode progress ticks.
- **Ending.** `do_batch_parent_things` settles the parent once its download is over: entries that never became a child are counted failed exactly once (`BatchRequest::settle_download`), and when `finished + failed` reaches the total the parent renders `BATCH_DONE`, archives, and leaves the queue. Cancelling the parent writes a `CANCEL` marker into every live child directory — by then the children are ordinary jobs and nothing else would stop them.

## Torrent routing

- `nyaaise()` classifies the URL into `TorrentType::{Link, Magnet, GDrive, Direct}`. Nyaa inputs are canonicalized to `/download/<id>.torrent` for the worker, while `display_source_link()` and Discord job embeds expose `/view/<id>` on the same Nyaa host.
- `TorrentType::GDrive` short-circuits `pn_dloadworker` to `pncurl --gscrape` (writes straight to `contents/torrent/input.mkv`) and skips the BitTorrent step.
- `TorrentType::Direct` handles direct HTTP(S) video file URLs (`.mkv`, `.mp4`, etc.) through `pncurl`, streams straight to `contents/torrent/input.mkv`, and skips the BitTorrent step.
- Torrent-backed `pnp2p` worker calls retain `--tag pandora-job-<job_id>` for protocol compatibility. `pnp2p` atomically locks each magnet BTIH or `.torrent` info-hash while downloading; a second process emits opcode `5` as `["5", "DUPLICATE_TORRENT", save_path]`. The download worker then announces `TORRENT_DUPLICATE_WAIT`, stores `Job.duplicate_source`, and waits until `DB/cache/inputs` has the matching input or the owning job's `contents/torrent/input.mkv` is ready and the owner has left active download/encode (`Encoded`/uploading/uploaded/terminal), then copies `input.mkv` into the requester and marks it `Downloaded`.
- Every encode input is copied into `DB/cache/inputs/<key>/input.mkv` when a job reaches `Encoded`, and every freshly downloaded preview input is cached when it reaches `Downloaded`; a cancelled job also caches if it was already past `Downloaded` (`Downloaded`, `Encoding`, `Encoded`, `Uploading`, `Uploaded`). Preview and encode jobs can therefore reuse the same source within the cache TTL. New uses reset the cache timer to 30 minutes (`INPUT_CACHE_TTL_SECS`). `pn_worker` runs `cleanup_torrent_runtime()` at startup to clear stale cross-process torrent locks, while valid input-cache entries survive; startup and the background cleanup tick evict only cache dirs whose `touch` file is older than the TTL. The input-cache key (`input_cache_key`) is `md5(torrent.get() | probe_file_index)`; `use_cache_or_wait` (run at download dispatch) first tries a cache copy, then falls back to waiting on an in-queue duplicate (`queued_duplicate_source`).
- `/probe` does **not** support GDrive or direct video links — `pn_probeworker` fails the job immediately.
- **Probe row order and paging**: `format_probe_rows` (probeworker) detects an episode number per file — first the direct `- 12` / `S01E12` / `E12` regexes, else `sequence_tokens`, which picks the numeric column that counts up across the file list — and, when at least two files match, sorts the rows by that number (`12v2` sorts after `12`; files with no number keep torrent order at the bottom). The displayed `` `n` `` stays the torrent's file index, since that is what `/encode pan` and the API's `file_index` select. `pnworker/probe_pages.rs` then chunks the rendered list into 10-line / 900-char pages (embed field values cap at 1024) and builds the `pnprobe:<job_id>:<page>` buttons; `Frontend::update` attaches them for `PROBE_ROW` and sends an empty component list for every other payload so stale buttons cannot survive on the message. `handlers/probe.rs::handle_probe_component` serves a page click by re-reading the full list from the job's `progress` JSON and rewriting the clicked embed, so paging survives a `pndc` restart and keeps no in-memory state.
- `/backup` does support GDrive and direct video links (re-upload to the configured Drive parent + skips streaming hosts via `--backup`).

## Parallel VerySlow encoding

Episode-scale measurement enables **chunking** only for veryslow-class presets: Dummy, Standard, and PseudoLossless use one continuous linear encoder because standalone chunking did not reduce their latency. Which presets those are is derived from the resolved preset (`ResolvedPreset::wants_chunked_encode` — an x264 preset of `veryslow` or `placebo`, or `chunked = true` in the file), not from a list of names, so a preset *file* gets the behaviour its own settings imply. VerySlow uses one single-threaded in-process libx264 encoder per logical worker, dynamic 250-frame work assignment, one concurrent AAC pass, numeric Annex-B concatenation, a video-only timestamp normalization pass, and then a stream-copy audio/video mux. The automatic worker count is capped to `floor(chunks / 4)`, so a 16-thread i9 uses 15 workers for a 61-chunk episode instead of rejecting parallel mode; inputs with fewer than four total chunks stay linear. `PN_PARALLEL_WORKERS` remains an upper-bound override.

The driver needs the input's frame total before it can lay out ranges, and the tail is what makes it exact: the last range asks the decoder for `total - last_start` frames and a chunk that comes up short fails the encode. That total used to come from `ffprobe -count_frames`, a full decode of the input run before the first worker starts, logging nothing while it ran — minutes of apparent silence between the worker fan-out line and the first frame. It now comes from the container: MP4's `nb_frames`, or the `NUMBER_OF_FRAMES` statistics tag mkvmerge writes (matched with or without its language suffix). A header value is taken only when the stream/format duration agrees with it to within 2%, so a statistics tag left behind by an earlier cut cannot truncate the tail; when it disagrees, or the container carries neither, the fallback is `-count_packets` — a demux, not a decode. The `parallel VerySlow done` run-log line names which of the three answered.

Standard, Dummy, and PseudoLossless still encode ahead while downloading, but do so through `pnmpeg --linear-prefix`: one persistent ffmpeg/libx264 process reads the verified growing-source pipe, applies the final ASS chain, and writes a video-only MP4 plus atomic `work/linear-aot.state` progress. At foreground handoff the ordinary pnmpeg invocation adopts that still-live process, starts the one AAC pass, relays its frame progress, waits for the same x264 instance to flush, and stream-copy muxes its video with audio. When the encode worker passes `--hls <dir>` — a release job on an HLS-only server that is not being kept locally — the job's last ffmpeg run writes the HLS layout into `work/hls` instead of an MP4, named from the encoded height and a fresh v4 UUID, and no `output.mp4` is produced at all. Which run that is depends on the job: the AOT handoff's own final mux, or, when the handoff is refused, the preset run itself, whose `+faststart` is dropped and whose output filename is rewritten to the HLS muxer's options. A job with an intro passes the flag to the concat run instead — the encode still has to leave a file the concat can read back, and the concat stream-copies, so it is the final mux. Only the VerySlow parallel encoder writes an MP4 the broker then remuxes. It never serializes or guesses at x264 internals: live CRF predictors, MB-tree, lookahead, and frame history stay inside the original process. A dead, stale, absent, or incompatible state falls back to the established full linear encode. GPU, Copy, and the downscaling 720p/480p presets do not use AOT by default; `Copy` never can, and the other two are a preset file's `aot = true` away from it.

### Background encodes

A job becomes background work in one of two ways: its preset declares `idle = true`, or it
**overtook** a job that was asked for before it (see below). Such a job is dispatched to a **second
encode lane** — `Worker::IdleEncode`, `enc-idle` — rather than to `enc-main`, and `pnmpeg` runs its
encode through the same gate the speculative planners use: the encode worker passes
`--aot-busyfile`/`--aot-lockfile` only for a job the coordinator marked background, and their
presence is the whole of how pnmpeg is told which kind of encode this is (`runs_idle_gated`). Which
jobs those are is decided in `do_job_progression_things` and travels as the last flag of
`WorkerMsg::Encode`, because it depends on the rest of the queue and not only on the preset.

The second lane is not an optimisation, it is what makes the feature possible. `Worker::Encode`
serves its jobs one at a time, so an idle encode dispatched there would be waiting for a quiet
machine while being the only reason it was busy, and would never pause. It also does not take
`ForegroundEncodeGuard`: the marker is what every idle encoder waits on, and one that published it
would be telling itself to stop. Between them, an ordinary encode ordered at any moment is
dispatched to a free `enc-main`, takes the marker, and the idle encode stops within about a
megabyte of source — roughly a second of media — because the gate is checked per buffer and the
feeder is backpressured by the encoder's own 64 KiB pipe.

**A pause costs nothing and a resume re-encodes nothing.** The gate stops the *feeder*, not the
encoder: ffmpeg blocks on an empty stdin with its rate control, lookahead and frame history intact,
and resumes from exactly where it stopped. The idle encode holds `.aot-owner` while running and
releases it while paused, so it neither multiplies the idle budget nor denies it to download-time
speculation for the hours it spends waiting.

An idle preset that also encodes ahead adopts its speculative prefix exactly as any other preset
does — that work was already gated, so re-doing it would spend the machine twice. Only when there is
nothing to adopt (a cached or duplicate input, `aot = false`, a planner that died) does pnmpeg run
the gated encoder itself, in-process, against a complete-from-the-start prefix sidecar describing
the finished input, and then adopt its own output through the ordinary handoff. That handoff emits a
progress frame every five seconds whether or not frames advanced, which is also what keeps a paused
encode from tripping the twenty-minute stall watchdog. A paused job reads as `enc-idle` at 0 fps.

Two limits are worth stating plainly. **The pause does not survive a pndc restart**: `fail_stale_active()`
fails every non-terminal job at startup and the encode begins again from zero, which for a
multi-hour encode under `start.sh`'s restart loop is a real cost. And an idle job is still an encode
job as far as `/gitsync` is concerned, so a pending gitsync waits for it and declines encodes
meanwhile — it resolves itself, since declining everything else is exactly the quiet the idle job
needs, but it resolves slowly.

### Opportunistic encodes

Two encodes ordered a minute apart do not download at the same speed, and the second one's input
often lands first. Handing it `enc-main` there makes the job that was asked for first wait out an
entire encode the moment its own download finishes; leaving the encoder alone until then wastes the
head start. So it takes neither: an encode dispatched while a job **ahead of it in the queue** is
still fetching its input (`awaits_its_input` — a `JobType::Encode`/`Pancode` at `Queued` or
`Downloading` that is not leased, forwarded or a batch parent) is dispatched to `enc-idle` with the
gate flags and `Job.opportunistic_encode` set. It encodes while the machine is otherwise quiet, and
stops within about a second of media when the job it overtook lands and takes `.foreground-encode`.
Nothing is re-encoded: the pause is the same feeder gate a background preset uses.

**A preset that chunks is never made opportunistic** (`preset::gateable_encode_for`). The gate
drives one linear encoder through a feeder it can stop; the parallel VerySlow scheduler is not one
encoder and has nothing to pause, and running it linearly to make it preemptible would cost more
encode time than stepping aside could ever save. Those keep `enc-main` and the ordering they have
always had. `Copy` resolves to no preset at all and is excluded by the same call.

**The last few minutes are waited out rather than stopped.** An opportunistic encode whose
remaining frames divided by its current fps come to under three minutes
(`nearly_finished_encode`, `OPPORTUNISTIC_FINISH_ETA`) holds `enc-main` closed for that long: the
job it overtook waits instead of parking a nearly-finished encode — its work directory, its message
and its queue slot — behind however long its own encode runs. The reading is in time rather than in
frames because ninety percent of a six-hour encode is still most of an hour. A reading older than
`OPPORTUNISTIC_PROGRESS_FRESH` is ignored, and a paused encode reports 0 fps, so neither a wedged
nor an already-stopped job can hold the encoder closed.

**Which presets encode ahead is decided by the preset, not by the CLI flag it was reached by and not by a table anywhere else** (`ResolvedPreset::wants_linear_aot`). That is what makes the rule survive a preset arriving as `--preset <name>` rather than as `--x264`, and what extends it to preset files.

Linear AOT is **codec-agnostic**: it is one ffmpeg process running the preset's own `-vf` chain and its own rendered encoder arguments (`ResolvedPreset::video_filter` and `video_encoder_args`), so libx264, NVENC, AMF, QSV and VAAPI presets all work through it unchanged. The argument list used to be reassembled from a fixed struct of x264 fields, which is the only reason it was ever libx264-only — and the filter was hardcoded to `ass=…,format=yuv420p`, which is why a scaling preset could not use it. Both now come from the preset, so a scaling preset encodes ahead at the height it asked for.

That makes encoding ahead a **preference**, and a preset file declares it with `aot`. The default is the set that has always done it — x264 presets that do not scale. A hardware preset stays off unless it asks, because the GPU is shared by the whole node and its foreground encode is minutes rather than hours; a scaling preset stays off because those are usually run beside a source-resolution encode. Chunking is not a preference in the same way: the chunk scheduler drives libx264 through `pnx264` and applies its own filter chain, so `ResolvedPreset::can_chunk` still hard-gates it to non-scaling x264 presets, and `chunked` only chooses within that. A preset that asks for chunks it cannot have is logged and encoded linearly rather than ignored in silence.

`ResolvedPreset::aot_compatibility` is what the speculative encode records and the foreground compares before adopting: the filter and the encoder arguments themselves, joined by a control character so an argument containing a space cannot forge a boundary. Anything omitted from that string is a way for two halves of one output file to be encoded differently with nothing downstream able to tell — the codec was omitted from the string this replaced, which was survivable only for as long as libx264 was the only one AOT could use.

**The coordinator asks the same preset.** `pnworker::core::download_aot_for` resolves the job's preset and reads `wants_linear_aot` / `wants_chunked_encode` off it, then passes the preset's *name* to the download worker, which spawns `pnmpeg --preset <name>`. It used to match on the `Preset` variant and pass a boolean flag, which made it a third copy of a table pnmpeg already had two of: a preset file that turned AOT on got a coordinator that never started one, and the speculative encode ran built-in settings whatever the file said.

Torrent, Direct, and Drive downloads can run pnmpeg's lookahead-only planner against `work/download.prefix`. It receives the same generated ASS filter input as the final encoder and flushes only actual IDR positions to `work/parallel.plan`; plain I frames are never boundaries. As lookahead closes ranges, the planner offers both natural-IDR and fixed 250-frame candidates to one shared disposable subprocess pool. The pool starts after four fixed ranges are known and ramps to at most one worker per four known fixed chunks. Each subprocess seeks only a range the continuous planner has already decoded, writes under `work/parallel-aot`, and atomically renames its output only after decoding the exact frame count. The download worker still stops the planner rather than waiting for it, but launched chunk subprocesses can finish independently.

At encode handoff, pnmpeg freezes speculative work with `parallel-aot/STOP`. A completed compatible plan selects natural ranges and reuses matching natural candidates; an incomplete, stale, underfilled, or incompatible plan selects the established fixed 250-frame fallback and can reuse matching fixed candidates. Cached/duplicate inputs naturally have no candidates. AOT is always opportunistic: a missing candidate is encoded normally and never changes whether the job succeeds.

AOT is globally idle-only. Every `enc-main` job holds `DB/work/.foreground-encode` for its whole worker invocation, including setup and concat. The marker carries both the pndc PID and job id: all other planners pause and release their lease, while the foreground job's own persistent linear encoder is allowed to finish its handoff. VerySlow planners launch no new chunks and let in-flight speculative chunks cancel atomically; continuous linear AOT stops feeding bytes and preserves its live x264 state. `DB/work/.aot-owner` gives only one download planner/encoder the idle AOT budget at a time, so concurrent downloads cannot multiply the CPU or memory cap. Crash-stale PID markers are reclaimed. When `enc-main` becomes idle, the owner resumes its same continuous state and pending work.

Parallel worker count is additionally capped from Linux `MemAvailable` after AOT processes are stopped. Defaults reserve 4096 MiB for the OS/other services and budget 800 MiB per 1080p VerySlow worker; `PN_PARALLEL_MEMORY_RESERVE_MIB` and `PN_PARALLEL_MEMORY_PER_WORKER_MIB` tune those safety estimates. This prevents CPU-count selection from invoking the OOM killer on a memory-constrained host.

### Reading an AOT failure

A speculative encoder that the kernel kills writes no message of its own, so the AOT paths record
what a post-mortem needs. Every `--linear-prefix` planner logs its start, a 15-second heartbeat
(`frames`, `bytes`, its own RSS, and host `MemAvailable`/`MemTotal`/`SwapFree`) written through a
second appending handle on `log/PNmpeg_Plan<job_id>.run.log` while the encode blocks, and a final
line carrying the same memory reading — a transcript that simply stops names the size the process
had reached and what the host had left at that moment. The download worker prints one
`[Pandora Downloader] job <id> AOT …` line to `pndc`'s stdout for every outcome: started (with the
planner PID), skipped (with which of the four reasons), left running for handoff, or stopped.

The frame total a handoff reports comes from the container header — duration times frame rate, two
metadata reads — and, when the input itself cannot be read, from a count the download worker recorded
for it. `record_total_frames` probes the selected file **before** renaming it to `input.mkv` and
leaves the number in `work/total_frames`, because after that rename a file the speculative encoder
holds open stops resolving by name: during an AOT handoff there is no readable path to the video at
all, so every probe of it fails and the progress the user watches has no denominator. The handoff
tries the input first and falls back to that sidecar; the log line names which one answered. Neither
comes from an exact `-count_packets` demux. The demux used to run on a thread for the
whole handoff, which put a third reader (with the speculative encoder and the AAC pass) on one file
on the bind mount, the slowest resource in the container, and still did not finish in time to report
a total: every AOT job reported zero for its whole run and drew no progress bar.

The exact count is now only paid for after the encode, and only when the header estimate and what the
AOT produced disagree by more than `TOTAL_ESTIMATE_TOLERANCE` (2 frames) — the case it exists to
catch. In the ordinary case it never runs at all. Only an exact count may reject a finished AOT on a
frame-count mismatch; an estimate is a frame or two out by nature and never fails a job on its own.

The exact count is held to that same 2-frame tolerance, and a mismatch **falls back to the linear
encode rather than failing the job**. The two numbers count different things: the AOT reports the
frames x264 emitted decoding a pipe through the filter chain, while `-count_packets` reports demuxed
video packets in the finished file, and a CFR-normalising chain on a non-seekable input can emit a
frame past the last packet. Held to exact equality, that rejected a job which had run to completion
and consumed every source byte for encoding 4496 of 4495 frames — and, alone among the handoff's
bail-outs, it failed the job outright instead of re-encoding. A truncated AOT, the thing the check
exists for, is short by thousands of frames and is still caught.

The AAC pass is started when the handoff begins and is watched as it runs. It used to be waited on
only after the video finished, so a handoff whose audio died in its first second still spent the whole
encode before anyone looked — one production run burned eleven minutes and 34,911 successfully encoded
frames before reporting that its input had not been there at all.

Its input frequently is not there, at the start. On the production bind mount a file the speculative
encoder holds open is **listed in its directory but cannot be stat'd by name** after the download
worker renames it to `input.mkv` (`exists=false` beside `torrent dir holds input.mkv` in the adoption
line). The name becomes usable again when that process exits, which is what the handoff is waiting
for anyway, so a failed audio pass is retried once the AOT completes rather than failing the job —
and only a name that still will not resolve ten seconds later is an error, reported with the
directory's actual contents. The same filesystem behaviour is why a job's log files could not be
stat'd while their writer was alive.

A missing state file is not by itself a failure. `LinearAotState::publish` replaces the file by
renaming a temporary over it, roughly once a second, and `DB` is a bind mount in production where
that rename is **not** atomic — the path goes briefly absent. The handoff polls it four times a
second, so a poll eventually lands in that window; four encodes died that way (2.9s, 58s, 177s and
337s in) while the publisher went on writing the same file for another quarter of an hour. The
handoff now waits out an absence for `MISSING_STATE_GRACE` (15s) before believing it, and if the
speculative process is gone by then it falls back to the ordinary linear encode instead of failing
the job.

When an absence does outlast the grace, the log records what else is still there — whether `work/`
and the job directory survive, and what the job directory still holds. One deleted file and a scratch
directory pulled out from under a running encode read identically otherwise, and only the second has
an external cause. `/gitsync` is that cause by design: it clears `DB/work` with no regard for what is
running, so it now prints the ids of the unfinished jobs it is about to break (`preserve_work_logs`
returns them), which is the only line connecting a deploy to the encodes that fail seconds later.

The foreground handoff logs to `log/PNmpeg_Encode<job_id>.run.log`: whether a state file was found
at all, the state it adopted, an incompatible key printed against the one this encode wanted, a
per-tick line beside each progress emit (frames, total or `counting`, fps, the AOT process's RSS,
host memory), and — the case that leaves nothing behind otherwise — the speculative process
vanishing, with the frames it reached, the last size it was seen at, and the memory reading. The
audio and mux children keep their stderr in `work/pnmpeg-linear-aot/`, and their failures quote its
last lines plus `killed by signal N` where a signal, not an exit code, ended them. Parallel VerySlow
logs the requested worker count, the count left after the `MemAvailable` headroom cap, and the
reading the cap was computed from.

Chunk decoders use input seeking with preserved source timestamps, so per-chunk libass evaluates episode-global event times. Audio is encoded once, not once per chunk. Raw H.264 begins with negative parser timestamps from B-frame reordering, so video is normalized separately before audio is muxed; applying the shift to both streams would introduce an audio delay.

## Server-scoped encode effects

Presets themselves may be **files**: `pnmpeg` takes `--preset <name>` and reads `DB/config/global/presets/<name>.toml` when one exists, falling back to its compiled-in table otherwise, so an untouched deployment encodes exactly as it always has and an operator opts one preset at a time out of the binary. A preset file also declares the `hardware` it needs, which is what keeps a GPU preset off a CPU-only Pandora Mini node (see [LINK.md](LINK.md#purpose)), and may declare `aot`, `chunked` and `idle` to override how the encode is scheduled. `av1` is a first-class AV1 NVENC preset with linear AOT enabled; `gpu` remains H.264. `presets/gpu-nvenc.toml` remains the NVENC replacement for the AMF `gpu` built-in, and `presets/cpu-x265.toml` is a libx265 preset — 10-bit HEVC at a 9000k average scaled to exactly 1920x1080, the only reference preset that encodes to a bitrate rather than a quality level, the only one that carries the source's metadata and chapters into the release, and the only one that declares `idle = true`. The field set is the union of what the built-in presets use plus an `extra_args` passthrough; the parameter *order* is fixed in code, because ffmpeg cares where the input, the maps and the output sit and a file that could reorder them would let a typo produce an encode that runs and is wrong. Reference copies of every built-in live in `presets/`, pinned by a unit test.

`Job::new` / `Job::new_api` snapshot the server's line-11 preset, line-12 intro group folder, line-19 outro group folder, and optional `DB/config/<server_id>/watermark.ass`. Both folders travel together inside the preset as one `Concat` value rather than as separate fields, so a preset that reaches the encoder carrying only half of what the server configured is not representable. Missing or invalid preset values fall back to Standard; missing or unregistered groups disable that end. Encode forwarding keys include the watermark hash and *both* concat folders, so a job with an outro never adopts the output of one without. The encode worker passes both folders and the resolved job preset to `pnmpeg`; `pnmpeg` stream-copies a matching retained variant per end or transcodes only that variant with the preset's video/audio encoder settings into a reusable compatibility variant, then joins intro, episode and outro in one stream-copy pass. The compatibility signature includes the target codec and stream properties, so AV1 and H.264 variants never share a cache entry. Because AV1 sequence headers can disagree beyond those probed fields, an AV1 candidate is also stream-copied with the episode and decoded across the boundary before it is trusted — in the order it will play, so an outro is tested after the episode rather than before it.

A concat pass runs whenever either end is configured, which is also what decides whether the encode may mux HLS itself: with a concat the encode writes `work/output_noconcat.mp4` and the concat writes the final layout, since the concat is as much a final mux as the encode. With neither, the encode writes the layout directly and its MP4 is promoted to `work/output.mp4`.

## Image watermark

The **logo** is a picture the encoder composites over every frame, beside the ASS watermark libass draws into the subtitle stream. A server may configure either, both, or neither; `/touchlogo` writes `DB/config/<serverid>/logo.toml` and the picture beside it. A logo may also be given a **period** — `5m:20s`, twenty seconds of logo out of every five minutes — which turns it from a fixture into a recurring burst that fades in and out at each end.

`Job::new` / `Job::new_api` snapshot it onto the job the same way as the ASS watermark and for the same reason — a logo replaced mid-queue must not retroactively change what a queued job ships — and job setup copies the picture *and* its placement into the job's own `contents/`. That copy is the single source both workers read: the download worker's speculative prefix and the foreground encode that adopts it each produce part of one video, and one of them reading the server's live config would put the logo in two different corners inside one episode.

`compose_logo_filter` appends `movie=<picture>[…];[base][logo]overlay=<x>:<y>` to the preset's own `-vf` chain, so the logo lands on the frame that is actually being encoded — after any scaling the preset does, which is what makes one stored placement correct at every resolution. It arrives through the `movie` source filter rather than a second `-i` because a simple filtergraph takes one decoder input, and a real input would mean rewriting every preset's `-map` arguments. Positions are nine anchors expressed with ffmpeg's `W`/`H`/`w`/`h` variables, so nothing is probed for them; only a percentage `width` consults `ffprobe`, against the preset's output width rather than the source's. `eof_action=repeat:shortest=0` keeps a still image on screen for the whole episode instead of ending the encode at frame one, and the chain's own pixel format is restated after the overlay — `overlay` given an alpha overlay negotiates an alpha format for the main input too, which on a 10-bit chain measured as `yuva420p`, adding a layer no release wants and dropping the video to 8 bits (libx265 then refuses the stream outright).

A **period** replaces the single overlay with a stack of them: the picture is `split` into `LOGO_FADE_STEPS` (8) branches, each mixed to its own fraction of the configured alpha by `colorchannelmixer`, and each `overlay` carries an `enable` expression naming the slice of the fade ramp that rounds to that level — once on the way in, once on the way out. Exactly one rung is ever enabled, and a timeline-disabled `overlay` hands the frame straight through, so the stack costs nothing during the minutes the logo is off screen. It is built this way because ffmpeg has no time-varying alpha for a still overlay: `colorchannelmixer` takes a number rather than an expression, and the per-pixel filter that does take one (`geq`) needs the logo turned into a looping video stream that `overlay`'s framesync then has to fast-forward through from zero — thousands of wasted frames for every chunk that starts late in the episode. The `enable` expressions are arithmetic on `mod(t,<every>)`, and `t` is the frame's own timestamp: the parallel chunk decoders seek with `-copyts`, so a chunk starting twenty minutes in shows the logo at exactly the moments a linear encode of the same episode would. The ramp is derived rather than configured — a quarter of the visible window, capped at a second — so a short appearance is not all fade.

All three encode paths apply it: the foreground `-vf` chain, the linear AOT prefix (`pnx264::linear`'s `filter`), and the parallel chunk decoders (`ParallelConfig::filter`, which is a parameter for this reason — it previously hardcoded the burned-in-subtitle chain, so a chunked preset file's own filter was ignored too). The logo is part of `aot_compatibility`, because it is part of the filter chain that key is computed over: without that, a prefix speculated before a logo was configured would be adopted into the first minutes of a release that has one. With no logo on either side the key is byte-identical to the preset's own, so nothing about an unwatermarked server changes. Only the encode burns it in — a concat that follows is a stream copy of a video the logo is already part of, and Keycode/Studio jobs snapshot no logo, exactly as they snapshot no ASS watermark.

Encode forwarding keys carry the picture's hash *and* its placement, the period included: two jobs that draw the same logo in different corners — or one on every frame and one in bursts — produce different videos and must not share an encode. A leased job carries the bytes and the placement in its spec — unlike a font or a concat group there is no corpus for a node to have synced, and the job already snapshotted it.

`pn_encdeworker` calls `server_effects` before pnmpeg. When a watermark exists, pnass injects it into a separate generated ASS and pnmpeg consumes that output. Injection appends watermark events after main subtitle events, performs the normal PlayRes/aspect-ratio and colliding-style checks, and maps `[all]` from zero through ASS's maximum timestamp; this renders identically to ending at the input duration while allowing the exact subtitle stream to be prepared before a download completes. `[precise]` and any other/empty Effect preserve their own timings. Injection writes `log/PNass_Inject<job_id>.log`. Failure terminates the job with `SERVER_EFFECTS_FAIL` carrying pnass's own opcode `2` reason, falling back to the `PNass_Inject<job_id>.log` path when pnass died without emitting one; cancellation remains cancellation. The untouched uploaded subtitle is retained so encoder reboot/retry cannot duplicate effects.

## Encode stall watchdog

The 160s shrine watchdog rescues the *layer*, not the job: it reboots a silent encoder and the epoch
reset hands the same job straight back to the new one. On a job that wedges the encoder every time —
an input ffmpeg will not finish, a tool that never emits — that pair is an infinite retry, silent,
with no terminal state and no message to the user, while the queue behind it never moves. Encode
stalls observed in production were all this shape.

`do_encode_stall_things` (`pnworker/core.rs`, run once per loop pass) closes it. A job is stalled
when it is a non-forwarded encode-type job, `encode_dispatched`, sitting at `Downloaded` or
`Encoding`, and its clock has run past `ENCODE_STALL_TIMEOUT` (**20 minutes**). The clock is
`encode_last_frame_at`, falling back to `encode_dispatched_at` before the first frame — so a slow
encode that keeps reporting frames is never touched, while one that goes quiet is caught whether it
died before `ENCODE_START` or halfway through.

On a stall the layer is force-rebooted first (so nothing else is dispatched into the same silent
task, and `kill_on_drop` takes the wedged tool down with it), then each stalled job fails with
`ENCODE_STALLED`: stage `Failed`, keep reservations released, forwarded children synced through
`sync_forwarded_jobs`, batch parent counters updated, archived and cleaned up like any other terminal
job. Its logs survive in `DB/saved_data/<job_id>/log`, readable with `/catlogs` or
`GET /api/v1/jobs/:id/logs` — a stalled job is now the *one* case that reliably keeps them, since a
job that stays in the queue loses them to the next `/gitsync`.

Archiving is the last moment that directory exists, since `cleanup_job` wipes the work directory
unconditionally. A plain `rename` is not enough to carry the logs out: it refuses a destination that
already holds files (a `publish.log`, or a gitsync that ran mid-job) and cannot cross a mount point,
and either way the transcript went into the wipe unreported — leaving a job that is archived, failed,
and completely undiagnosable. It now prints why the directory move was refused, then falls back to
moving the files one at a time, reading the whole listing before it moves anything (taking entries
out of a directory mid-walk is a bad bet on `DB`'s bind mount) and printing what it could not keep.

Reading is forgiving in the same direction: `log_files` skips an entry it cannot stat, names it on
stdout, and fails the request only when nothing readable turned up. One unresolvable name in the
directory must never answer `500` in place of every transcript sitting beside it.

## Linked nodes (Pandora Mini)

`pndc --mini` runs the worker runtime with a link client in place of the Discord client, taking
whole jobs from a coordinating `pndc`. On the coordinator, `do_link_things` (run once per loop pass,
before the stall watchdog) is the entire lifecycle of a remote job: every local dispatch skips a job
whose `link_node` is set, so nothing else can touch it.

- **Offload happens at submit**, in `try_link_offload`, before `queue_new_job` — everything that
  function does (preparing a work directory, dispatching a download) is what the node will do
  instead. A job that finds no free node falls straight through and runs locally, so a full,
  drained or absent cluster is never a reason for work to wait. Subtitle normalisation still runs
  here, so a node can never be the thing that discovers an attachment was a PGS stream.
- **A node forwards payloads, not summaries.** `lifecycle::render` is tapped on the node side —
  not the `CommData` stream, because declines and cancellations never reach it — and the coordinator
  replays each payload through `persist_side_effects` and `render`. A remote job therefore needs no
  rendering path of its own and localises against its own `lang`.
- **Requeue, not recovery.** A remote job's inputs are a link and a few KB of subtitle, so a lost
  lease, an abandoned node or a declined job all return it to the queue as an ordinary local
  candidate. `LINK_MAX_ATTEMPTS` (2) stops a poisoned job touring the cluster.
- **A leased job is never a duplicate source.** Its input was downloaded on the node, so this
  machine's copy of its work directory is empty.
- **Logs come to the coordinator.** A node ships each of a job's tool logs forward as it grows, on
  every renew, and the coordinator appends them into its own `DB/work/<job>/log` — so `/catlogs`
  and the API log routes answer for a remote job through `lib::joblog` unchanged, and `cleanup_job`
  archives them like any other. A terminal job flushes what is left before its result ends the
  lease.
- **Upload policy travels with the job.** A node holds no `meta.pandora` for the originating guild,
  so `drive_only` is resolved here and passed to its upload worker as an override. An HLS-only job
  is encoded remotely but published here: the node stops at `Encoded`, hands the MP4 back, and the
  coordinator resumes the job from there so the playback capability lives on the public hostname.

Full protocol, token tier, offload rules and failure handling in [LINK.md](LINK.md).

## Worker snapshot

The queue is a `Vec<Job>` owned by `pn_worker` and shrine heartbeats are in-memory, so none of the
state a stall lives in reaches the database. Once a second the loop publishes a JSON snapshot of both
to `pnworker/snapshot.rs` (a `RwLock` behind a `OnceLock`, one writer, no channel that could block
the loop); `GET /api/v1/workers` serves it. See [API.md](API.md#worker-snapshot).

## Encode forwarding

A second **API** encode job (`Frontend::Web`, `JobType::Encode`) whose `encode_forward_key` matches a non-terminal parent already in the queue is *forwarded* instead of re-run — it skips download/encode/upload entirely and mirrors the parent's outcome. The key (`encode_forward_key`) is a versioned `md5` over `[source, probe_file_index, preset, md5(attachment), md5(server_watermark), server_id, gdrive_folder_global, gdrive_folder_local]` (source via `encode_source_key`: gdrive/direct link / magnet info-hash / `.torrent` info-hash / raw link). `mark_forwarded` sets `job.forward_parent`, worker `enc-forward`, and the job's stage to the parent's; `persist_forwarded_wait` writes progress JSON `{type:"forward", parent_job_id}`. `sync_forwarded_jobs` propagates the parent's every stage transition (and terminal archive/cleanup) to all of its forwarded children. Discord jobs are never forwardable (only `Frontend::Web`). The web renders the `forward` progress type as an indeterminate "shared with job #N — reuses that encode" pipeline state.

## Adding a new TorrentType variant

If you extend `TorrentType` in `lib::p2p/nyaaise.rs`, you must also update:

- `impl TorrentType` (`get`, `get_arg`, `display`).
- `nyaaise()` classifier.
- Every match block in `pnworker/workers/downloadworker.rs` and `probeworker.rs`.
- The exhaustive match blocks in `nyaaise.rs`'s `#[cfg(test)] mod tests`.
