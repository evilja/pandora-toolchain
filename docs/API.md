# API.md

HTTP API routes, auth/tokens, web console behavior, and deployment.

`src/lib/http/api/` exposes the worker over HTTP so a website (or scripts) can submit/inspect/cancel jobs against the same backend as Discord. `pndc::main` spawns `lib::http::api::serve(tx.clone(), port)` when `api_port` is non-zero, sharing the worker's `Sender<JobClass>` — API submits land in the same `channel(5)` queue, so there is one job pipeline, not two.

## HTTP API config

Key consts in `lib::env/standard.rs`:

- `api_port` enables the API server when set and non-zero;
- `api_host` is the bind address (defaults to `0.0.0.0`, set `127.0.0.1` to keep it loopback-only behind a proxy);
- `api_author_id` is the Discord user id stamped as author on API-submitted jobs;
- `api_public_url` is the public origin Pandora is reachable on (the Cloudflare tunnel hostname, no trailing slash). It is what makes a batch encode a one-message job: with it set, `/encode batch` links its output page instead of posting a Discord message per episode — see [WORKER.md](WORKER.md#batch-encodes);
- `api_rate_limit` (`API_RATE_LIMIT`, default `1000`) and `api_rate_window_secs` (`API_RATE_WINDOW_SECS`, default `60`) configure the per-token write-request rate limit.

API bearer tokens live one-per-line in `DB/config/global/environment/api.pandora` (`API_TOKENS_PATH`); blank lines and `;`-prefixed lines are ignored. Mint tokens with `/gentoken`. Not committed.

## Auth

- `Authorization: Bearer <token>` checked against the lines of `api.pandora` (blanks and `;` comments ignored) by an axum middleware layered on the `/api/v1` routes. The page routes (`GET /`, `/jobs`, `/encode`, `/settings`, `/git`, `/studio`, `/trace`, `/console.css`, `/console.js`, `/favicon`, `/favicon.ico`) and `/health` are unauthenticated; every operation they submit, including tracing and ASS export, is protected. `GET /batch/:token` and `GET /batch/:token/output` are authorized by the 256-bit capability in the path — the link is posted to Discord, where nobody holds an API token — and expose nothing but that one batch; `GET`/`HEAD /lumiere/v1/files/:token/:filename` is separately authorized by a memory-only 256-bit capability, restricted to the exact registered file, range-capable, non-cacheable, and removed when the corresponding upload ends. `GET`/`HEAD /lumiere/v1/hls/:token/*resource` uses a persisted 256-bit capability restricted to the generated layout: two `.m3u8` playlists and either MPEG-TS chunks (`chunk-<height>p/p<n>-<uuid>.ts`) or an fMP4 init segment plus CMAF media fragments (`chunk-<height>p/init-<uuid>.mp4` and `chunk-<height>p/p<n>-<uuid>.m4s`). Every name uses the source's height and one random v4 UUID; the capability is non-cacheable, CORS-readable, survives restarts, and expires after 12 hours.
- **Rate limit**: the same `auth` middleware rate-limits **write** requests only (any method that isn't `GET`/`HEAD`, so status polling is never throttled), keyed by an md5 of the token (`ApiRateLimiter` in `core.rs`). Default `1000` requests per `60`s sliding window, configurable via `api_rate_limit` / `api_rate_window_secs`. On exceed it returns `429` with a `Retry-After` header (seconds until the window resets) and body `"rate limit exceeded"`. The web consoles read `Retry-After` and render a friendly "rate limit hit — try again in Ns" notice on `429`.
- **PNwitch tokens**: `parse_token_file` also keeps the `;` comment line preceding a token as its **label** (`/gentoken label:<note>` writes `; <label> (added <unix>)`), and `require_pnwitch` restricts the operator-only endpoints — `POST /gitsync`, `POST /acix/publish`, `GET /workers`, and the job-log routes — to a token labelled exactly `PNwitch`. It is the API's stand-in for the Discord Witch tier; every other token gets `403 PNwitch token required`.
- **Link tokens**: a token line may be `<token>|link|<node_name>|<purpose>` (mint with `/gentoken link:<node> purpose:<cpu|gpu|both>`). The optional fourth field is what the node is *for* and is the first CPU/GPU scheduling rule — a `gpu` preset is never offered to a node marked `cpu`. GPU/Both nodes additionally report hardware encoders that succeeded on real test frames, and a GPU preset is offered only when that list contains its exact codec. Purpose comes from the token rather than from anything the node reports, so a machine cannot promote itself into GPU work; an absent or unrecognised value is `cpu`, which is what every token minted before the field existed means. It authorises a Pandora Mini node and opens **only** the `/api/v1/link/*` routes — no job submits, no logs, no git. `require_link(&auth)` returns the bound node name, and every link route additionally checks that the node named in the request body matches it, so one node cannot renew or finish another's lease. Link routes are also exempt from the write rate limit: a node renews every lease every ten seconds for as long as it works, which is a heartbeat on a fixed cadence rather than user traffic. See [LINK.md](LINK.md).
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
- `POST /api/v1/link/register`, `GET /api/v1/link/lease`, `POST /api/v1/link/lease/:id/renew`, `POST /api/v1/link/lease/:id/result`, `GET /api/v1/link/release`, `GET /api/v1/link/assets/manifest`, `GET /api/v1/link/assets/:hash` (link token only — see [LINK.md](LINK.md)). `GET /link/release` answers `{ version, build, commit, reset }` — what the coordinator is running, which is how a node knows it is behind. It is a poll of its own rather than a field on an existing answer because an idle node only ever calls `GET /link/lease`, which long-polls and returns `204` with no body. The asset routes serve the font/intro corpus a node syncs; `:hash` is a content hash and is served only when the current manifest lists it, so a node can never address a path on the coordinator's disk.

Subtitles travel as base64 (`subtitle_b64`), decoded by a local `base64_decode_bytes`; `gitcode` fetches the subtitle from `subtitle_url` (GitHub blob links auto-rewritten to raw). Either may carry ASS or any text subtitle format ffmpeg can read — the worker normalises it to ASS when the job is queued (see [DISCORD.md](DISCORD.md#subtitle-formats)); image-based or non-UTF-8 payloads decline the job with that reason instead of failing later in the encoder. `pancode` takes `probe_job_id` as a **string** (job ids exceed JS's safe-integer range) + a `file_index`, looks up the probe job's torrent from the DB, and builds a `Pancode` job. `encode` and `gitcode` accept an optional `preset` naming the encoder for that one job — `standard`, `veryslow`, `gpu`, `av1`, `pseudolossless`, `dummy`, `720p`, or `480p` (case-insensitive; `very_slow` and `pseudo_lossless` also parse) — overriding the server default for that request only; an unrecognised name is a `400`, and the server's intro group is kept either way because it belongs to the server rather than to the preset. AV1 requests return `400` unless the bound server is either HLS-only or Drive-only; HLS AV1 is emitted as fMP4/CMAF. `720p` and `480p` are the standard preset with the frame height capped there (never upscaled), and they are the only way to reach those presets: `/edit` does not offer them. Pancode, git-smartcode, and Studio requests accept no preset/concat controls at all. Without a `preset`, local-token jobs derive preset and concat from the bound server's `/edit` settings, while jobs without a server id use Standard with no intro. Submits return `202 { job_id }`. Cancel first DB-checks the target: it requires a local token, refuses cross-server jobs (`row.server_id != token.local_server_id`), accepts `Encode`, `Studio`, and `StudioPreview` jobs, refuses archived/terminal jobs, then sends `HalfJob(Cancel)` and returns `202`. Exposed over the API: encode/backup/probe/pancode/gitcode (jobs), the full Studio workflow (local-token only), init/attach/source/detach/destruct/smartcode (git, local-token only — see above), and `gitsync` (`POST /api/v1/gitsync`). **Not** exposed: `/configure`, `/edit`, `/job`, `/hearts`, translation commands, `!auth`/`!ban` — they need richer Discord guild context, Discord attachments, or the live shrine handle.

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

`nodes` is the Pandora Mini roster, which is in-memory for the same reason the queue is: a node's
liveness is nowhere in the jobs table. `purpose` is what the node's token marks it as and decides
which presets it is offered; `build` is the release it last recorded itself level with, so a number
that has stopped moving is a node failing to update; `migration_error` is a migration it could not
run, which is otherwise invisible because such a node still takes work and still looks healthy. Queue entries carry `link_node` and `link_attempts` — a job
with `link_node` set is executing on that node and progresses only through the link.

```json
{
  "updated_at": 1787003523, "queue_len": 1, "gitquery_pending": false, "encode_reboot_count": 0,
  "hearts": [{ "worker": "Encode", "alive": true, "last_beat_secs": 3, "reboot_count": 0 }],
  "nodes": [{
    "node": "mini-osaka", "threads": 16, "max_jobs": 1,
    "encoders": ["h264_nvenc", "av1_nvenc"], "drain": false,
    "last_seen_secs": 4, "jobs": ["4242"], "pandora_version": "3.5.0-lumiere",
    "encoder_identity": "x264-165-0.165.x-pandora", "purpose": "gpu", "build": 41,
    "migration_error": null
  }],
  "queue": [{
    "job_id": "4242", "job_type": "Encode", "stage": "Downloaded", "worker": "lnk-mini-osaka",
    "server_id": null, "forward_parent": null, "link_node": "mini-osaka", "link_attempts": 0,
    "batch_parent": null, "waiting_on_cache": false,
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

A job that ran on a Pandora Mini node keeps its logs here too: the node ships each log forward as it
grows and the coordinator appends into that job's own `DB/work/<job_id>/log`, so these routes and
`/catlogs` answer for a remote job with no new endpoint and no new token tier. See
[LINK.md](LINK.md#log-shipping).

Both frontends share `lib::joblog`: `find_job_logs` looks in `DB/work/<job_id>/log` first (`"location": "active"`) and falls back to `DB/saved_data/<job_id>/log` (`"archived"`) for a job whose lifecycle already moved it, takes plain files only (no subdirectories), and sorts them by name. A job with neither directory — or with an empty one — is a `404 no logs for this job` on every route.

### `publish.log`

The publish commands are not jobs and so wrote nothing anywhere: a `/akiraconfirm` that hung left no trace, and a failed reply edit meant the run was invisible from both Discord and the API. `lib::publishlog` closes that gap. Every publish path — `/publish`, `/acixconfirm`, `/acixunpublish`, `/akiraconfirm`, `/openanimeconfirm`, `/anizmconfirm` — appends to a **`publish.log`** in the log directory of the job it was given, so it lists and reads through the three routes above with no new endpoint or token tier.

Each line is `<unix seconds> [<command>] <event>`. Newlines in the event are collapsed to spaces so one event stays one greppable line (provider errors routinely arrive as multi-line API bodies). Two events are always present: an `invoked by user <id> in channel <id>` line written as soon as the `job_id` parses, and the command's final user-facing message — the funnel that edits the Discord reply logs its content first, so **every** outcome is recorded, including validation errors and provider failures. When the reply edit itself fails, a `response edit failed: <error>` line follows it; that is what separates "the command never ran" from "the command ran and only the reply was lost".

Because publishing happens long after the encode finished, the job's logs have usually already been archived — `publish_log_path` appends beside whichever copy exists and creates `DB/work/<job_id>/log` only when the job kept no logs at all, since a job with no logs is exactly the one that is otherwise undiagnosable. Logging is best-effort: a write failure prints to stderr and is swallowed, never failing the publish.

- `GET /api/v1/jobs/:id/logs` — the listing: `{ job_id (string), location, total_bytes, files: [{ name, bytes, modified (unix seconds) }], job }`. `job` is the same `JobStatus` object `GET /jobs/:id` returns (stage, worker, progress, links) or `null` when the row is gone, so one request shows both where the job stopped and what it wrote.
- `GET /api/v1/jobs/:id/logs/:name` — one log file as `text/plain; charset=utf-8`, `Cache-Control: no-store`. `:name` must equal a name from the listing — that exact-match lookup is also the path-traversal guard — otherwise `404`. Encoder logs are unbounded, so the read is **from the end**: `?max_bytes=` (default 1 MiB, capped at 8 MiB) seeks that far back and drops the partial first line, and `?tail=<lines>` further narrows it to the last N lines. `X-Pandora-Log-Bytes` is the full on-disk size and `X-Pandora-Log-Truncated` says whether the byte cap cut anything, so a caller can tell a short log from a trimmed one. Invalid UTF-8 is replaced, never an error.
- `GET /api/v1/jobs/:id/logs.zip` — every log file of that job as `application/zip`, `Content-Disposition: attachment; filename="pandora-logs-<id>.zip"`, built by the same `zip_log_files` the Discord command uses. Unlike `/catlogs` there is no 24 MiB ceiling: that limit is a Discord attachment constraint, not a Pandora one.

## Progress & links

The worker chokepoint in `pnworker/core.rs` (`persist_side_effects`) writes structured JSON to the DB as side effects of the normal `CommData` stream — `ENCODE_PROG`/`ENCODE_CONCAT_PROG` → `progress` (`{type:"encode", frame, total, fps, kbps, percent}`), `PROBE_ROW` → `progress` (`{type:"probe", files, file_options}`, holding the whole episode-sorted list — Discord pages that same string, the web renders all of it), and `UPLOAD_DONE`/`UPLOAD_BACKUP_PROG`/`BACKUPALL_PROG` at stage Uploaded → `uploaded_links` (host→url map). Completed local keeps replace progress with `{type:"keep", keyword, parent_keyword, kind, expires_at, ready:true}`; the web job view displays those details and the recent-jobs table includes the output keyword. Download progress is `{type:"download", percent, done, total}`; the **cache/duplicate** behaviour is also surfaced — a job waiting on an in-flight duplicate input persists `{type:"download", waiting:"cache"}` (written from `use_cache_or_wait` at dispatch and from the `TORRENT_DUPLICATE_WAIT` branch in `core.rs`), and a cache hit / resolved duplicate copy persists `{type:"download", percent:100, cached:true}`. For uploads, `progress.hosts` is the positional per-host array `[drive, byse, lulustream, voe, hls]` (`upload_payload`) — HLS reuses the retired index-4 host slot, so private Drive metadata stays at positions 5+ for compatibility with finished jobs. Each scheduled external slot holds an in-flight progress string (e.g. `"Byse 11/1032 MB"`) until that host finishes, when it becomes the host's URL. When the server enables `/edit hls:true`, all first four slots remain empty and `hls` contains the 12-hour Lumiere Files master capability URL, making it the job's sole advertised link. Without HLS-only mode, `hls` is blank; the three external streaming-host slots are empty when the server's `drive_only` policy is enabled. `GET /api/v1/jobs/:id` surfaces both `progress` and `uploaded_links`; the web renders a blue progress bar for encode **and** upload jobs (the upload segment fills with the live `percent`, not a static full bar), a "waiting on a cached input" readout for the cache-wait state (and a "shared with job #N" readout for a `{type:"forward"}` job), the probe file list, and the upload links **inline as each host completes** (parsed straight from `progress.hosts`, so they appear during the upload). Upload links render like Discord: plain clickable URL lines with no host prefix/left label; when the current upload payload contains only final URLs, the web hides the `100%` text. The web shows no separate "Links" section for upload jobs — only `uploaded_links` of non-upload jobs (e.g. backup_all `episodes`) get the `linksBlock`.

## Job construction

API jobs are built with `Job::new_api(...)` → `Frontend::Web`, so they run through the worker with no Discord context (see [WORKER.md](WORKER.md)).

## Web pages

All dependency-free, `include_str!`/`include_bytes!`-baked into `pndc`, same origin as `/api/v1` so no CORS; editing any requires rebuilding `pndc`):

- `GET /`, `/jobs`, `/encode`, `/settings` → the console (`web/index.html`), which picks its view from `location.pathname`
- `GET /git` → Repositories (`web/git.html`)
- `GET /studio` → Studio Cutroom, the browser-native nonlinear editor (`web/studio.html`)
- `GET /trace` → Kagami Trace Lab (`kagami-trace/web/index.html`)
- `GET /batch/:token` → batch output page (`web/batch.html`), capability-authorized
- `GET /console.css` → the shared design system (`web/shell.css`)
- `GET /console.js` → the shared shell: rail, topbar, theme, token, API wrapper, pipeline rendering (`web/shell.js`)
- `GET /studio-sw.js` → the Studio editor's authenticated media-stream bridge
- `GET /favicon` (+ `/favicon.ico`) → site icon

The consoles fetch relative `/api/v1` paths. Details in `web/README.md`.

## Batch output page

`GET /batch/<token>` serves `web/batch.html` and `GET /batch/<token>/output` serves its data; both are unauthenticated but require the batch's 256-bit token, minted per batch job and mapped to the parent job id under `DB/config/global/batch/<token>`, so the link survives a `pndc` restart. The token is validated (64 hex chars, known mapping) before either route answers, and an unknown token is a `404` on both.

The page is deliberately static HTML that fetches its data separately with `cache: 'no-store'`, and the data route sets `Cache-Control: no-store`: Cloudflare will happily cache an HTML document for a job that is still running. `/output` returns the parent's stage and counters plus one entry per episode — label, paired subtitle name, child `job_id`, and the child's live `stage`, `worker`, `progress`, and `uploaded_links` joined from the jobs table — so a finished episode keeps showing its links after the batch itself is archived. The page renders that as near-plaintext, colours the stage, and polls every 5s until every episode is terminal.

## Console shell

Every page is the same frame: a fixed 224px rail (wordmark, the seven destinations, a health
line) and a topbar (page title, per-page actions, an API-connection light, and a token chip
linking to Settings). `web/shell.css` (`GET /console.css`) holds the design tokens and every
shared component; `web/shell.js` (`GET /console.js`) draws the rail and topbar from one `NAV`
table and exposes `PN.*` — `api`, `bar`, `stepper`, `chip`, `icon`, `toast`, `getToken`/`setToken`,
`setTheme`, `refreshIdentity`. A page ships `.pn-app > .pn-main > .pn-content` and calls
`PN.shell({ page, title, actions })` once. **Changing shared chrome means editing those two
files, not four HTML documents** — that is the point of serving them rather than inlining them.

`kagami-trace/web/index.html` injects both only when `pandoraMode` is true (it is served at
`/trace`), so standalone `pntrace` requests neither and the sub-crate stays extraction-ready.

## Pipeline rendering

Two devices, both derived from `stage` + `progress`, both in `shell.js`:

- `PN.bar(stage, progress, withPercent)` — one blue track, filled by `overallPercent()` (completed
  phases plus the live one). This is what every table row shows. Tone goes green when the job
  finished, red when it failed, grey when it was cancelled; **progress itself is never green**.
- `PN.stepper(stage, progress)` — the vertical Queue / Download / Encode / Upload list on a job's
  detail panel, each row Completed / In progress / Pending / Failed. A probe job gets a two-step
  Queue / Probe list instead.

`PN.chip(stage)` is the status dot — Active, Queued, Completed, Failed, Cancelled — and
`PN.chip(stage, true)` prints the raw stage name with the same colour. Under the stepper the
console's own `readout()` prints what an encoder actually watches:
`41% · frame 18422/34071 · 41.2 fps · 4210 kbit/s · ETA 13m`.

`PN.routeText()` renders the Encode form's expected route as `Download → Encode → Upload`.

## Deep links

- `GET /jobs?job=<id>` opens the Jobs page with that job selected in the detail panel. Operations
  rows and the "Recently finished" entries link here; so does a queued Smartcode from Repositories.

The old `?embed=1`, `?job=`, `?jobs=1` modes and the draggable job windows were removed with the
desktop shell.

## Console views

`web/index.html` serves four views off `location.pathname`:

- **Operations** (`/`) — four stat tiles, a Live pipeline table of ongoing jobs, a Worker capacity
  panel from `GET /workers`, and Recently finished from `?status=recent`. Elapsed comes from the
  worker snapshot's `secs_since_request`, so it is blank without a PNwitch token; `GET /workers`
  is operator-only, so the capacity panel renders a "needs a PNwitch token" state otherwise.
- **Jobs** (`/jobs`) — All/Active/Queued/Completed/Failed tabs over `?status=recent`, a client-side
  search on job id and source, and a sticky detail panel that polls the selected job every 2s and
  carries **Cancel job** (which needs a local token and a non-terminal job on that token's server).
- **Encode** (`/encode`) — tabs for `Encode` / `Git Encode` / `Backup` / `Pancode` / `Keycode`, with
  a Submission summary beside them (token reach, workers online, queue depth, and the expected route
  as `Download → Encode → Upload`)
  and preset guidance that changes with the selected preset. Encode and Git Encode carry a `Preset`
  dropdown defaulting to `Server default` (an empty value, which sends no `preset` at all) and are
  the only interface offering the `720p` and `480p` presets. Pancode keeps its two-step probe →
  pick a file → encode flow. Submitting locks the page to that job until it ends.
- **Settings** (`/settings`) — the bearer token (saved to `localStorage` `pandora_token`, shared by
  every console), its probed reach, **Forget saved token** (this browser) vs **Revoke on the server**
  (`POST /token/revoke`, two-step confirm), theme, and the job-poll preference.

## Repositories

`web/git.html` (`GET /git`): the git endpoints (`Init`/`Attach`/`Source`/`Smartcode`/`Detach`/`Destruct`/`Credits/Readme`); Smartcode derives preset/concat from the server's `/edit` settings; **local token required** (renders the `403` specially for a plain token). The page is selection-driven: an **Attached anime** table (`GET /git/attachments`, searchable) picks
the repo, and a details card below it carries Source and Smartcode as tabs plus **Detach** and
**Destruct** in its header. Destruct asks you to type the anime's name back before it deletes the
Forgejo repo. **New repository** (init) and **Attach existing** open a form from the topbar and pick
their channel from a live Discord channel dropdown (`GET /git/channels`, last pick remembered in
`localStorage`) — no raw ids are ever typed. A **README template** card at the bottom auto-loads
`base.md`, shows the formatting guide when none is set, and saves via `POST /git/readmebase`.

## Theme

The palette is taken from the reference designs: a flat near-black ground (`#05101d` content,
`#091524` rail, `#0e1927` cards), one blue accent (`#2562c3` buttons, `#3c81eb` links, progress
and icons), and green / amber / red reserved for status only. Radii are small (6px, 8px for
cards), there are no drop shadows on panels, and the only serif on any page is the `PANDORA`
wordmark — page titles and stat values are the body sans, bold.

All of it lives in `web/shell.css` as `--pn-*` tokens: `:root` is light,
`:root[data-theme="dark"]` is dark, and **that file is the only place to retheme**. Studio and the
Trace Lab map their own local variables onto `--pn-*` rather than carrying palettes of their own.

`pandora_theme` in `localStorage` holds `dark` / `light` / `system` (default `system`); `shell.js`
resolves it to a `data-theme` attribute on load, follows `prefers-color-scheme` while set to
system, and repaints on the `storage` event so a change in Settings reaches other open tabs. The
choice is made in Settings — there is no titlebar toggle any more.

## Favicon

`GET /favicon` serves a bundled circular icon (`web/favicon.png`, `include_bytes!`), overridable at runtime by `DB/config/global/favicon.{png,ico,svg,jpg,jpeg,webp,gif}` (first match wins, content-type by extension).

Every page is responsive: below 1080px the rail becomes a horizontal icon bar and the two-column
layouts stack; below 700px the topbar's page action is dropped in favour of the rail. Keyboard focus
is visible throughout and `prefers-reduced-motion` collapses every transition.

## Deployment

`Dockerfile` (multi-stage — builds all workspace bins, runtime image bundles `ffmpeg`) + `docker-compose.yml` run `pndc` alongside a `cloudflared` sidecar on a shared network with **no published ports**; the Cloudflare tunnel's public-hostname service points at `http://pndc:8787` (the compose service name, not `localhost`). The build downloads the Pandora x264 fork's source archive at a hard-pinned commit, verifies its SHA-256 and plan-only fork marker, and compiles the static library inside the same `rust:1-bookworm` stage that links pnmpeg. Building source in Bookworm prevents a newer host glibc from leaking unresolved `__isoc23_*` references into the archive; a source digest mismatch, missing fork marker, retained diagnostic log, or incompatible symbol fails the image build. Updating the fork requires reviewing and changing both pinned `PNX264_SOURCE_URL` and `PNX264_SOURCE_SHA256` defaults in `Dockerfile`; Compose needs no x264 release variables. `DB/` is bind-mounted so the database, env, and tokens persist. Lumiere remote uploads may use a second public Tunnel hostname pointing at the same service; that hostname must allow provider access to `/lumiere/v1/files/*` and player access to `/lumiere/v1/hls/*`. See `web/README.md` and [LUMIERE_BROKER.md](LUMIERE_BROKER.md).
