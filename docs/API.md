# API.md

HTTP API routes, auth/tokens, web console behavior, and deployment.

`src/lib/http/api/` exposes the worker over HTTP so a website (or scripts) can submit/inspect/cancel jobs against the same backend as Discord. `pndc::main` spawns `lib::http::api::serve(tx.clone(), port)` when `api_port` is non-zero, sharing the worker's `Sender<JobClass>` — API submits land in the same `channel(5)` queue, so there is one job pipeline, not two.

## HTTP API config

Key consts in `lib::env/standard.rs`:

- `api_port` enables the API server when set and non-zero;
- `api_host` is the bind address (defaults to `0.0.0.0`, set `127.0.0.1` to keep it loopback-only behind a proxy);
- `api_author_id` is the Discord user id stamped as author on API-submitted jobs;
- `api_public_url` is the public origin Pandora is reachable on (the Cloudflare tunnel hostname, no trailing slash). It is what makes a batch encode a one-message job: with it set, `/encode batch` links its output page instead of posting a Discord message per episode — see [WORKER.md](WORKER.md#batch-encodes);
- `api_rate_limit` (`API_RATE_LIMIT`, default `30`) and `api_rate_window_secs` (`API_RATE_WINDOW_SECS`, default `60`) configure the per-token write-request rate limit.

API bearer tokens live one-per-line in `DB/config/global/environment/api.pandora` (`API_TOKENS_PATH`); blank lines and `;`-prefixed lines are ignored. Mint tokens with `/gentoken`. Not committed.

## Auth

- `Authorization: Bearer <token>` checked against the lines of `api.pandora` (blanks and `;` comments ignored) by an axum middleware layered on the `/api/v1` routes. The page routes (`GET /`, `/encode`, `/git`, `/studio`, `/trace`, `/favicon`, `/favicon.ico`) and `/health` are unauthenticated; every operation they submit, including tracing and ASS export, is protected. `GET /batch/:token` and `GET /batch/:token/output` are authorized by the 256-bit capability in the path — the link is posted to Discord, where nobody holds an API token — and expose nothing but that one batch; `GET`/`HEAD /lumiere/v1/files/:token/:filename` is separately authorized by a memory-only 256-bit capability, restricted to the exact registered file, range-capable, non-cacheable, and removed when the corresponding upload ends. `GET`/`HEAD /lumiere/v1/hls/:token/*resource` uses a persisted 256-bit capability restricted to the three name shapes an output publishes — `<height>p_<uuid>.m3u8`, `<height>p_<uuid>_variant.m3u8`, and `chunk-<height>p/p<n>-<uuid>.ts`, all built from the source's height and one random v4 UUID; it is non-cacheable, CORS-readable, survives restarts, and expires after 12 hours.
- **Rate limit**: the same `auth` middleware rate-limits **write** requests only (any method that isn't `GET`/`HEAD`, so status polling is never throttled), keyed by an md5 of the token (`ApiRateLimiter` in `core.rs`). Default `30` requests per `60`s sliding window, configurable via `api_rate_limit` / `api_rate_window_secs`. On exceed it returns `429` with a `Retry-After` header (seconds until the window resets) and body `"rate limit exceeded"`. The web consoles read `Retry-After` and render a friendly "rate limit hit — try again in Ns" notice on `429`.
- **PNwitch tokens**: `parse_token_file` also keeps the `;` comment line preceding a token as its **label** (`/gentoken label:<note>` writes `; <label> (added <unix>)`), and `require_pnwitch` restricts the operator-only endpoints — `POST /gitsync`, `POST /acix/publish`, `GET /workers`, and the job-log routes — to a token labelled exactly `PNwitch`. It is the API's stand-in for the Discord Witch tier; every other token gets `403 PNwitch token required`.
- **Local tokens**: a token line in `api.pandora` may be `<token>|local|<server_id>` (mint with `/gentoken local`). `api_auth_for_token` parses it into `ApiAuth { local_server_id }`; `effective_server_id` makes a local token force its `server_id` onto job submits. The **git endpoints require a local token** — `require_local(&auth)` returns `403` for a plain token, since repo ops need a server to resolve the Forgejo org config and per-channel meta. API cancel also requires a local token and only allows cancelling non-terminal `Encode` jobs whose persisted DB `server_id` equals the token's `local_server_id`.

## Git routes

Local token only; under `/api/v1/git/`:

- `GET /git/attachments`
- `GET /git/channels`
- `GET /git/readmebase`
- `POST /git/readmebase`
- `POST /git/{init,attach,source,detach,destruct,smartcode}`

`GET /git/readmebase` returns the server's README template `{ content, is_guide:false }` from `DB/config/<server_id>/base.md`, falling back to the operator guide `DB/config/global/base.md` then the bundled `lib::git::README_BASE_GUIDE` (`src/lib/git/readme_guide.md`) as `{ content, is_guide:true }`; `POST /git/readmebase` writes `{ content }` to `DB/config/<server_id>/base.md` (mirrors the Discord `/readmebase`). They call `lib::git` and run synchronously. `detach` removes the channel meta (repo untouched); `destruct` deletes the Forgejo repo and detaches; `smartcode` merges the channel's TL(+TS) for an episode (`lib::git::smartcode_merge` — ports the pnass `--merge` pipeline: fetch TL/TS, optional `--split-signs`, `--merge`, upload `Release - …`, write `SOURCE.md`), then builds a `Job::new_api(Encode)` from the merged bytes + resolved link and submits it to the worker queue (so it returns `202 { job_id, link, release_path, warnings }`, not a synchronous result). API smartcode uses the same named local Drive cleanup path as Discord smartcode: after a later successful upload for the same episode, the previous stored Drive file is deleted and the stored file/folder IDs are replaced. API smartcode does not do acix publishing (Discord-only). `GET /git/attachments` returns the token's server's attached animes (scans `DB/config/<server_id>/*/meta.toml` via `lib::git::list_attachments`) as `[{ channel_id (string), mal_id, name, slug, kind, episode_count, season, repo_url }]`, sorted by name. `GET /git/channels` returns the server's live Discord channel list as `[{ id (string), name, kind }]` by reading `DB/config/<server_id>/channels.json` (the `pndc` event handlers publish this — see [PROJECT.md](PROJECT.md)); returns `[]` if the file is absent. The git console uses attachments to pick a channel by anime (Source) and channels to pick any channel (Init/Attach), so no raw ids are typed. `server_id` comes from the local token; the request body carries `channel_id` (a **string**, Discord snowflakes exceed JS safe ints), `mal`, optional `season` + `tl`/`tlc`/`ts`/`qc` credits (`attach` also `repo`; `source` takes `episode` + `link`). On success `init`/`attach` return `200` with `{ owner_repo, repo_url, name, slug, kind, episode_count, season, created, renamed_files }`; `source` returns `{ path, content }`.

## Studio routes

All Studio routes require a local token. The token supplies the guild and `api_author_id` supplies the collaborator identity, so Discord and HTTP can operate on the same Studios. A user can own multiple Studios, while one guild/user pointer identifies the current Studio used by editing and render routes. API responses omit server filesystem paths; Discord snowflakes in `collaborators` and submitted `channel_id` values are strings.

- `GET /api/v1/studios` — list every unexpired Studio owned by the API user; each object has `current`.
- `POST /api/v1/studios` with `{ keywords: [string, ...] }` — create and select a Studio without leaving previously owned Studios.
- `GET /api/v1/studios/current` / `GET /api/v1/studios/:id` — current or owned-Studio details, including sources, tracks, media metadata, collaborators, and timestamps.
- `POST /api/v1/studios/:id/switch` — select an already-owned Studio. `POST /api/v1/studios/:id/reown` joins/selects a previous or shared guild Studio.
- `POST /api/v1/studios/current/disown` — leave only the current Studio; other owned Studios remain available to switch to.
- `POST /api/v1/studios/current/keywords` with `{ keywords: [...] }` — atomically replace source keeps.
- `POST /api/v1/studios/current/tracks` — add audio with `{ audio_b64, filename, mode, duck_volume_percent?, fade_seconds? }`; `mode` is `insert`, `override`, or `duck`.
- `POST /api/v1/studios/current/tracks/:track_id/{edit,move,cut,remove}` — edit fields (`mode`, `volume_percent`, `duck_volume_percent`, `fade_seconds`), move with `{ offset }`, cut with `{ side, seconds }`, or remove.
- `GET /api/v1/studios/current/media/sources/:source_index` / `GET /api/v1/studios/current/media/tracks/:track_id` — authenticated, range-addressable media streams for the browser editor. Source indexes are zero-based. Both return `Accept-Ranges: bytes`, validate current-Studio collaboration, and never expose filesystem paths.
- `POST /api/v1/studios/current/timeline` — return the current timeline as `image/png`.
- `POST /api/v1/studios/current/preview` with `{ track_id?, position?, duration_seconds?, channel_id? }` / `POST /api/v1/studios/current/render` with `{ channel_id? }` — snapshot and queue a `StudioPreview` or `Studio` job, returning `202 { job_id }`. A preview needs at least one of `track_id` or `position` (`start`, `middle`, or `end`); `duration_seconds` is from 1 to 300 and defaults to 32 seconds for a bare `track_id` and 30 seconds otherwise. The anchoring rules match `/studio preview` (see [DISCORD.md](DISCORD.md)). `channel_id`, when supplied, is a numeric string. The preview route remains available for Discord/API compatibility; the Studio webpage never calls it.

`volume_percent` runs from 0 to 500. Audio files are limited to 50 MiB each and may be in any format ffmpeg can decode (see [DISCORD.md](DISCORD.md#studio-audio-formats)); the media-stream routes label known audio containers with their real content type so the browser editor can decode them. Because uploads are base64 inside JSON, the add-track route accepts request bodies up to 70 MiB to carry a 50 MiB file plus base64 expansion; the decoded file size is checked separately, while all other protected routes retain the 8 MiB request-body limit. The webpage streams the base video through a same-origin service worker that supplies bearer auth, decodes audio assets with Web Audio, and performs insert/override/duck preview mixing locally—seeking or editing does not create server jobs. Only Deliver calls the final render route. Explicit API preview/final jobs use `Frontend::Web`, the same worker pools, server preset rules, immutable render snapshots, progress DB, and job-status endpoints as their Discord equivalents.

## Trace routes

Any API token may use the tracing routes; a local token is not required. `POST /api/v1/trace` accepts an encoded image as its raw request body and the same `preset`, tracing-option, and `svg_seam_overlap` query fields as standalone `pntrace`, returning `{ trace, svg, elapsed_ms }`. `POST /api/v1/trace/ass` accepts `{ trace, filename?, duration_centiseconds?, seam_overlap? }` and returns a ZIP containing exactly one libkagami-generated ASS file. Both routes run through the standard bearer-auth and write-rate-limit middleware. The static lab lives at `GET /trace`, stores `pandora_token` with the other consoles, and sends it as a bearer token; standalone `pntrace` retains its unauthenticated loopback `/api/trace` and `/api/ass` routes.

## Routes

- `GET /api/v1/jobs` (all non-archived; `?status=ongoing` filters to non-terminal — used by the console's job dropdowns; `?status=recent` returns the last 50 jobs including archived ones, which is how you find the id of a job that already ended)
- `GET /api/v1/jobs/:id`
- `POST /api/v1/jobs/encode`
- `POST /api/v1/jobs/backup`
- `POST /api/v1/jobs/probe`
- `POST /api/v1/jobs/pancode`
- `POST /api/v1/jobs/gitcode`
- `POST /api/v1/jobs/:id/cancel`
- `GET /api/v1/jobs/:id/logs`, `GET /api/v1/jobs/:id/logs.zip`, `GET /api/v1/jobs/:id/logs/:name` (PNwitch token only — see [Job logs](#job-logs))
- `GET /api/v1/workers` (PNwitch token only — see [Worker snapshot](#worker-snapshot))
- `POST /api/v1/token/revoke` (any token — see [Token revocation](#token-revocation))

Subtitles travel as base64 (`subtitle_b64`), decoded by a local `base64_decode_bytes`; `gitcode` fetches the subtitle from `subtitle_url` (GitHub blob links auto-rewritten to raw). Either may carry ASS or any text subtitle format ffmpeg can read — the worker normalises it to ASS when the job is queued (see [DISCORD.md](DISCORD.md#subtitle-formats)); image-based or non-UTF-8 payloads decline the job with that reason instead of failing later in the encoder. `pancode` takes `probe_job_id` as a **string** (job ids exceed JS's safe-integer range) + a `file_index`, looks up the probe job's torrent from the DB, and builds a `Pancode` job. Encode, pancode, git-smartcode, and Studio requests do not accept preset/concat controls: local-token jobs derive them from the bound server's `/edit` settings, while jobs without a server id use Standard with no intro. Submits return `202 { job_id }`. Cancel first DB-checks the target: it requires a local token, refuses cross-server jobs (`row.server_id != token.local_server_id`), accepts `Encode`, `Studio`, and `StudioPreview` jobs, refuses archived/terminal jobs, then sends `HalfJob(Cancel)` and returns `202`. Exposed over the API: encode/backup/probe/pancode/gitcode (jobs), the full Studio workflow (local-token only), init/attach/source/detach/destruct/smartcode (git, local-token only — see above), and `gitsync` (`POST /api/v1/gitsync`). **Not** exposed: `/configure`, `/edit`, `/job`, `/hearts`, translation commands, `!auth`/`!ban` — they need richer Discord guild context, Discord attachments, or the live shrine handle.

## Token revocation

`POST /api/v1/token/revoke` — **any token**, no label or local binding required. It revokes the token the request presents, and only that one: being able to prove you hold a token is the whole authority needed to throw it away, so a token issued for one piece of work can be handed back without `/gentoken` rights or a hand-edit of `api.pandora`.

It deletes the token's line from `api.pandora` **and the `;` label comment directly above it** — `parse_token_file` reads that comment as the token's label, so leaving it behind would silently relabel whichever token came next. A `<token>|local|<server_id>` line is matched on its token part. Returns `200 { "revoked": true, "lines_removed": N }`. Because `api_auth_for_token` caches on the file's mtime, the very next request with that token is `401`; every other token is untouched.

## Worker snapshot

`GET /api/v1/workers` — **PNwitch token only.** The live state of the worker loop, which is nowhere
in the jobs table: shrine heartbeats and the in-memory queue. This is what tells you a job is stuck
*while it is stuck*, instead of reconstructing it afterwards from a row whose stage
`fail_stale_active()` has already overwritten.

`pn_worker` publishes it once a second (`pnworker/snapshot.rs`); the route reads the last publish and
never blocks the loop. `503 worker has not published a snapshot yet` means the API is up but the
worker loop is not — `serve` is its own task, so that is a real and distinguishable state.

```json
{
  "updated_at": 1787003523, "queue_len": 1, "gitquery_pending": false, "encode_reboot_count": 0,
  "hearts": [{ "worker": "Encode", "alive": true, "last_beat_secs": 3, "reboot_count": 0 }],
  "queue": [{
    "job_id": "4242", "job_type": "Encode", "stage": "Downloaded", "worker": "enc-main",
    "server_id": null, "forward_parent": null, "batch_parent": null, "waiting_on_cache": false,
    "encode_dispatched": true, "encode_dispatch_order": 1, "encode_dispatch_epoch": 3,
    "secs_since_dispatch": 91, "secs_since_frame": null, "secs_since_request": 140,
    "encode_frame": null, "encode_total": null, "encode_fps": null
  }]
}
```

The stall signature reads directly off it: `encode_dispatched: true` with a climbing
`secs_since_dispatch` and a null `secs_since_frame` is an encoder that took the job and went silent;
a climbing `hearts[Encode].last_beat_secs` and `reboot_count` is the layer being rebooted under it.
`ENCODE_STALL_TIMEOUT` fails such a job at 20 minutes (see [WORKER.md](WORKER.md#encode-stall-watchdog)).

## Job logs

The HTTP counterpart of Discord's `/catlogs` (see [DISCORD.md](DISCORD.md)), for reading a job's tool logs when it is stuck or failed. All three routes require a **PNwitch-labelled token** — logs carry filesystem paths, torrent names, and upload URLs, so they sit at the same tier as the Discord command — and all are `GET`, so they are never rate-limited.

Both frontends share `lib::joblog`: `find_job_logs` looks in `DB/work/<job_id>/log` first (`"location": "active"`) and falls back to `DB/saved_data/<job_id>/log` (`"archived"`) for a job whose lifecycle already moved it, takes plain files only (no subdirectories), and sorts them by name. A job with neither directory — or with an empty one — is a `404 no logs for this job` on every route.

### `publish.log`

The publish commands are not jobs and so wrote nothing anywhere: a `/akiraconfirm` that hung left no trace, and a failed reply edit meant the run was invisible from both Discord and the API. `lib::publishlog` closes that gap. Every publish path — `/publish`, `/acixconfirm`, `/acixunpublish`, `/akiraconfirm`, `/openanimeconfirm`, `/anizmconfirm` — appends to a **`publish.log`** in the log directory of the job it was given, so it lists and reads through the three routes above with no new endpoint or token tier.

Each line is `<unix seconds> [<command>] <event>`. Newlines in the event are collapsed to spaces so one event stays one greppable line (provider errors routinely arrive as multi-line API bodies). Two events are always present: an `invoked by user <id> in channel <id>` line written as soon as the `job_id` parses, and the command's final user-facing message — the funnel that edits the Discord reply logs its content first, so **every** outcome is recorded, including validation errors and provider failures. When the reply edit itself fails, a `response edit failed: <error>` line follows it; that is what separates "the command never ran" from "the command ran and only the reply was lost".

Because publishing happens long after the encode finished, the job's logs have usually already been archived — `publish_log_path` appends beside whichever copy exists and creates `DB/work/<job_id>/log` only when the job kept no logs at all, since a job with no logs is exactly the one that is otherwise undiagnosable. Logging is best-effort: a write failure prints to stderr and is swallowed, never failing the publish.

- `GET /api/v1/jobs/:id/logs` — the listing: `{ job_id (string), location, total_bytes, files: [{ name, bytes, modified (unix seconds) }], job }`. `job` is the same `JobStatus` object `GET /jobs/:id` returns (stage, worker, progress, links) or `null` when the row is gone, so one request shows both where the job stopped and what it wrote.
- `GET /api/v1/jobs/:id/logs/:name` — one log file as `text/plain; charset=utf-8`, `Cache-Control: no-store`. `:name` must equal a name from the listing — that exact-match lookup is also the path-traversal guard — otherwise `404`. Encoder logs are unbounded, so the read is **from the end**: `?max_bytes=` (default 1 MiB, capped at 8 MiB) seeks that far back and drops the partial first line, and `?tail=<lines>` further narrows it to the last N lines. `X-Pandora-Log-Bytes` is the full on-disk size and `X-Pandora-Log-Truncated` says whether the byte cap cut anything, so a caller can tell a short log from a trimmed one. Invalid UTF-8 is replaced, never an error.
- `GET /api/v1/jobs/:id/logs.zip` — every log file of that job as `application/zip`, `Content-Disposition: attachment; filename="pandora-logs-<id>.zip"`, built by the same `zip_log_files` the Discord command uses. Unlike `/catlogs` there is no 24 MiB ceiling: that limit is a Discord attachment constraint, not a Pandora one.

## Progress & links

The worker chokepoint in `pnworker/core.rs` (`persist_side_effects`) writes structured JSON to the DB as side effects of the normal `CommData` stream — `ENCODE_PROG`/`ENCODE_CONCAT_PROG` → `progress` (`{type:"encode", frame, total, fps, kbps, percent}`), `PROBE_ROW` → `progress` (`{type:"probe", files, file_options}`, holding the whole episode-sorted list — Discord pages that same string, the web renders all of it), and `UPLOAD_DONE`/`UPLOAD_BACKUP_PROG`/`BACKUPALL_PROG` at stage Uploaded → `uploaded_links` (host→url map). Completed local keeps replace progress with `{type:"keep", keyword, parent_keyword, kind, expires_at, ready:true}`; the web job view displays those details and the recent-jobs table includes the output keyword. Download progress is `{type:"download", percent, done, total}`; the **cache/duplicate** behaviour is also surfaced — a job waiting on an in-flight duplicate input persists `{type:"download", waiting:"cache"}` (written from `use_cache_or_wait` at dispatch and from the `TORRENT_DUPLICATE_WAIT` branch in `core.rs`), and a cache hit / resolved duplicate copy persists `{type:"download", percent:100, cached:true}`. For uploads, `progress.hosts` is the positional per-host array `[drive, byse, lulustream, voe, hls]` (`upload_payload`) — HLS reuses the retired index-4 host slot, so private Drive metadata stays at positions 5+ for compatibility with finished jobs. Each scheduled external slot holds an in-flight progress string (e.g. `"Byse 11/1032 MB"`) until that host finishes, when it becomes the host's URL. When the server enables `/edit hls:true`, all first four slots remain empty and `hls` contains the 12-hour Lumiere Files master capability URL, making it the job's sole advertised link. Without HLS-only mode, `hls` is blank; the three external streaming-host slots are empty when the server's `drive_only` policy is enabled. `GET /api/v1/jobs/:id` surfaces both `progress` and `uploaded_links`; the web renders a karaoke-gradient bar for encode **and** upload jobs (the upload segment fills with the live `percent`, not a static full bar), an indeterminate "waiting on a cached input" bar for the cache-wait state (and the same indeterminate bar for a `{type:"forward"}` job, captioned "shared with job #N"), the probe file list, and the upload links **inline as each host completes** (parsed straight from `progress.hosts`, so they appear during the upload). Upload links render like Discord: plain clickable URL lines with no host prefix/left label; when the current upload payload contains only final URLs, the web hides the `100%` text. The web shows no separate "Links" section for upload jobs — only `uploaded_links` of non-upload jobs (e.g. backup_all `episodes`) get the `linksBlock`.

## Job construction

API jobs are built with `Job::new_api(...)` → `Frontend::Web`, so they run through the worker with no Discord context (see [WORKER.md](WORKER.md)).

## Web pages

All dependency-free, `include_str!`/`include_bytes!`-baked into `pndc`, same origin as `/api/v1` so no CORS; editing any requires rebuilding `pndc`):

- `GET /` → desktop shell (`web/desktop.html`)
- `GET /batch/:token` → batch output page (`web/batch.html`), capability-authorized
- `GET /encode` → encode console (`web/index.html`)
- `GET /git` → git console (`web/git.html`)
- `GET /studio` → browser-native nonlinear Studio editor (`web/studio.html`)
- `GET /trace` → Kagami raster-to-vector lab (`kagami-trace/web/index.html`)
- `GET /studio-sw.js` → the Studio editor's authenticated media-stream bridge
- `GET /favicon` (+ `/favicon.ico`) → site icon

The consoles fetch relative `/api/v1` paths. Details in `web/README.md`.

## Batch output page

`GET /batch/<token>` serves `web/batch.html` and `GET /batch/<token>/output` serves its data; both are unauthenticated but require the batch's 256-bit token, minted per batch job and mapped to the parent job id under `DB/config/global/batch/<token>`, so the link survives a `pndc` restart. The token is validated (64 hex chars, known mapping) before either route answers, and an unknown token is a `404` on both.

The page is deliberately static HTML that fetches its data separately with `cache: 'no-store'`, and the data route sets `Cache-Control: no-store`: Cloudflare will happily cache an HTML document for a job that is still running. `/output` returns the parent's stage and counters plus one entry per episode — label, paired subtitle name, child `job_id`, and the child's live `stage`, `worker`, `progress`, and `uploaded_links` joined from the jobs table — so a finished episode keeps showing its links after the batch itself is archived. The page renders that as near-plaintext, colours the stage, and polls every 5s until every episode is terminal.

## Desktop shell

`web/desktop.html` (`GET /`): a small window manager over the consoles. A bottom **taskbar** has Encode/Git/Studio/Trace/**Jobs** launchers, a clock, an **API-token toggle button** (a popover whose password input writes the shared `localStorage` `pandora_token`), and the **☾/☀ theme toggle**. Each app opens as a draggable/**resizable** window whose body is an `<iframe>` to its `?embed=1` page; windows have **traffic-light controls** (red = close, yellow = maximize, green = minimize), z-stacking on focus, and their open state + geometry persist in `localStorage` (`pandora_desktop_v1`). On mobile (≤760px) the WM is replaced by a launcher card linking to the standalone consoles. The desktop keeps its **own** copy of the `:root` theme vars for its chrome (a third place to retheme, alongside the two consoles).

## Embed & job-only modes

Consoles, including the Trace lab, support:

- `?embed=1` adds `html.embed`, which drops the outer titlebar/border/shadow, fills the iframe (the command grid flex-grows so the footer pins to the bottom), and hides the footer token field — the desktop taskbar owns the token, and the consoles **live-sync** it via the `storage` event.
- `?job=<id>` adds `html.jobonly` and renders only that job's live pipeline (no command UI), used by desktop job windows.
- `?jobs=1` (also `html.jobonly`) renders only the live recent-jobs table, used by the desktop **Jobs** window; its rows/⤢ pop individual job windows.

## Job windows / auto-pop

Submitting a job (encode/gitcode/backup/pancode) **auto-pops** it into its own window instead of rendering inline in the console output (which just shows a short "popped out" note). When embedded, the console `postMessage`s `{type:"pandora:openJob", jobId}` to the desktop, which opens a desktop job window (iframe `/encode?embed=1&job=<id>`); standalone it pops a local floating `.jobwin`; mobile falls back to inline watching. The Jobs table rows and a ⤢ "pop out" button pop windows too. The console's **Jobs** command, when embedded, posts `{type:"pandora:openJobs"}` so the desktop opens a single **Jobs** window (`/encode?embed=1&jobs=1`) instead of rendering the table inline; standalone it renders inline as before.

## Encode console

`web/index.html` (`GET /encode`): left command list (`Encode`/`Git Encode`/`Backup`/`Pancode`/`Jobs`/`Cancel`), right options, footer with token + Run, karaoke-style pipeline view of a job's stages.

## Git console

`web/git.html` (`GET /git`): the git endpoints (`Init`/`Attach`/`Source`/`Smartcode`/`Detach`/`Destruct`/`Credits/Readme`); Smartcode derives preset/concat from the server's `/edit` settings; **local token required** (renders the `403` specially for a plain token). Source/Smartcode/Detach/Destruct pick the channel from a live attached-anime dropdown (`GET /git/attachments`); Init/Attach from a live Discord channel dropdown (`GET /git/channels`) — both refreshable, last pick remembered in `localStorage`, no raw ids typed. **Credits/Readme** edits the server's README template (`base.md`) inline: it auto-loads on select (no Run needed), shows the formatting guide when none is set, and **Run saves** via `POST /git/readmebase`.

## Theme

Pandora (Re:Zero) palette — `:root` light + `:root[data-theme="dark"]` dark, `pandora_theme` in `localStorage`, applied by an inline `<head>` script before first paint. The two consoles share an **identical `<head>`** (CSS + scripts) — `git.html` is regenerated from `index.html`'s head, so retheme `index.html` then re-sync (only the `<title>` should differ); `desktop.html` has its own head. The titlebar toggle shows ☀ in light mode and ☾ in dark. The standalone consoles listen for `pandora_theme` storage events, and the desktop pushes theme changes into already-open same-origin iframe windows so the inner consoles repaint immediately. Traffic-light colors are theme variables: light mode is the swapped/opposite ordering of dark mode (`r` bright / `y` medium / `g` muted in light; `r` muted / `y` medium / `g` bright in dark).

## Favicon

`GET /favicon` serves a bundled circular icon (`web/favicon.png`, `include_bytes!`), overridable at runtime by `DB/config/global/favicon.{png,ico,svg,jpg,jpeg,webp,gif}` (first match wins, content-type by extension).

Both consoles are responsive (mobile breakpoint at 760px, full-bleed window + 16px inputs to avoid iOS zoom).

## Deployment

`Dockerfile` (multi-stage — builds all workspace bins, runtime image bundles `ffmpeg`) + `docker-compose.yml` run `pndc` alongside a `cloudflared` sidecar on a shared network with **no published ports**; the Cloudflare tunnel's public-hostname service points at `http://pndc:8787` (the compose service name, not `localhost`). The build downloads the Pandora x264 fork's source archive at a hard-pinned commit, verifies its SHA-256 and plan-only fork marker, and compiles the static library inside the same `rust:1-bookworm` stage that links pnmpeg. Building source in Bookworm prevents a newer host glibc from leaking unresolved `__isoc23_*` references into the archive; a source digest mismatch, missing fork marker, retained diagnostic log, or incompatible symbol fails the image build. Updating the fork requires reviewing and changing both pinned `PNX264_SOURCE_URL` and `PNX264_SOURCE_SHA256` defaults in `Dockerfile`; Compose needs no x264 release variables. `DB/` is bind-mounted so the database, env, and tokens persist. Lumiere remote uploads may use a second public Tunnel hostname pointing at the same service; that hostname must allow provider access to `/lumiere/v1/files/*` and player access to `/lumiere/v1/hls/*`. See `web/README.md` and [LUMIERE_BROKER.md](LUMIERE_BROKER.md).
