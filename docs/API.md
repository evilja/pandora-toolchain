# API.md

HTTP API routes, auth/tokens, web console behavior, and deployment.

`src/lib/http/api/` exposes the worker over HTTP so a website (or scripts) can submit/inspect/cancel jobs against the same backend as Discord. `pndc::main` spawns `lib::http::api::serve(tx.clone(), port)` when `api_port` is non-zero, sharing the worker's `Sender<JobClass>` — API submits land in the same `channel(5)` queue, so there is one job pipeline, not two.

## HTTP API config

Key consts in `lib::env/standard.rs`:

- `api_port` enables the API server when set and non-zero;
- `api_host` is the bind address (defaults to `0.0.0.0`, set `127.0.0.1` to keep it loopback-only behind a proxy);
- `api_author_id` is the Discord user id stamped as author on API-submitted jobs;
- `api_rate_limit` (`API_RATE_LIMIT`, default `30`) and `api_rate_window_secs` (`API_RATE_WINDOW_SECS`, default `60`) configure the per-token write-request rate limit.

API bearer tokens live one-per-line in `DB/config/global/environment/api.pandora` (`API_TOKENS_PATH`); blank lines and `;`-prefixed lines are ignored. Mint tokens with `/gentoken`. Not committed.

## Auth

- `Authorization: Bearer <token>` checked against the lines of `api.pandora` (blanks and `;` comments ignored) by an axum middleware layered on the `/api/v1` routes. The page routes (`GET /`, `/encode`, `/git`, `/favicon`, `/favicon.ico`) and `/health` are unauthenticated.
- **Rate limit**: the same `auth` middleware rate-limits **write** requests only (any method that isn't `GET`/`HEAD`, so status polling is never throttled), keyed by an md5 of the token (`ApiRateLimiter` in `core.rs`). Default `30` requests per `60`s sliding window, configurable via `api_rate_limit` / `api_rate_window_secs`. On exceed it returns `429` with a `Retry-After` header (seconds until the window resets) and body `"rate limit exceeded"`. Both web consoles read `Retry-After` and render a friendly "rate limit hit — try again in Ns" notice on `429`.
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
- `POST /api/v1/studios/current/preview` with `{ track_id, channel_id? }` / `POST /api/v1/studios/current/render` with `{ channel_id? }` — snapshot and queue a `StudioPreview` or `Studio` job, returning `202 { job_id }`. `channel_id`, when supplied, is a numeric string. The preview route remains available for Discord/API compatibility; the Studio webpage never calls it.

Audio uploads are base64 inside JSON and therefore share the protected router's 8 MiB request-body limit. The webpage streams the base video through a same-origin service worker that supplies bearer auth, decodes audio assets with Web Audio, and performs insert/override/duck preview mixing locally—seeking or editing does not create server jobs. Only Deliver calls the final render route. Explicit API preview/final jobs use `Frontend::Web`, the same worker pools, server preset rules, immutable render snapshots, progress DB, and job-status endpoints as their Discord equivalents.

## Routes

- `GET /api/v1/jobs` (all non-archived; `?status=ongoing` filters to non-terminal — used by the console's job dropdowns)
- `GET /api/v1/jobs/:id`
- `POST /api/v1/jobs/encode`
- `POST /api/v1/jobs/backup`
- `POST /api/v1/jobs/probe`
- `POST /api/v1/jobs/pancode`
- `POST /api/v1/jobs/gitcode`
- `POST /api/v1/jobs/:id/cancel`

Subtitles travel as base64 (`subtitle_b64`), decoded by a local `base64_decode_bytes`; `gitcode` fetches the subtitle from `subtitle_url` (GitHub blob links auto-rewritten to raw). `pancode` takes `probe_job_id` as a **string** (job ids exceed JS's safe-integer range) + a `file_index`, looks up the probe job's torrent from the DB, and builds a `Pancode` job. Encode, pancode, git-smartcode, and Studio requests do not accept preset/concat controls: local-token jobs derive them from the bound server's `/edit` settings, while jobs without a server id use Standard with no intro. Submits return `202 { job_id }`. Cancel first DB-checks the target: it requires a local token, refuses cross-server jobs (`row.server_id != token.local_server_id`), accepts `Encode`, `Studio`, and `StudioPreview` jobs, refuses archived/terminal jobs, then sends `HalfJob(Cancel)` and returns `202`. Exposed over the API: encode/backup/probe/pancode/gitcode (jobs), the full Studio workflow (local-token only), init/attach/source/detach/destruct/smartcode (git, local-token only — see above), and `gitsync` (`POST /api/v1/gitsync`). **Not** exposed: `/configure`, `/edit`, `/job`, `/hearts`, translation commands, `!auth`/`!ban` — they need richer Discord guild context, Discord attachments, or the live shrine handle.

## Progress & links

The worker chokepoint in `pnworker/core.rs` (`persist_side_effects`) writes structured JSON to the DB as side effects of the normal `CommData` stream — `ENCODE_PROG`/`ENCODE_CONCAT_PROG` → `progress` (`{type:"encode", frame, total, fps, kbps, percent}`), `PROBE_ROW` → `progress` (`{type:"probe", files}`), and `UPLOAD_DONE`/`UPLOAD_BACKUP_PROG`/`BACKUPALL_PROG` at stage Uploaded → `uploaded_links` (host→url map). Completed local keeps replace progress with `{type:"keep", keyword, parent_keyword, kind, expires_at, ready:true}`; the web job view displays those details and the recent-jobs table includes the output keyword. Download progress is `{type:"download", percent, done, total}`; the **cache/duplicate** behaviour is also surfaced — a job waiting on an in-flight duplicate input persists `{type:"download", waiting:"cache"}` (written from `use_cache_or_wait` at dispatch and from the `TORRENT_DUPLICATE_WAIT` branch in `core.rs`), and a cache hit / resolved duplicate copy persists `{type:"download", percent:100, cached:true}`. For uploads, `progress.hosts` is the positional per-host array `[drive, doodstream, lulustream, voe, abyss]` (`upload_payload`): each slot holds an in-flight progress string (e.g. `"Doodstream 11/1032 MB"`) until that host finishes, when it becomes the host's URL. `GET /api/v1/jobs/:id` surfaces both `progress` and `uploaded_links`; the web renders a karaoke-gradient bar for encode **and** upload jobs (the upload segment fills with the live `percent`, not a static full bar), an indeterminate "waiting on a cached input" bar for the cache-wait state (and the same indeterminate bar for a `{type:"forward"}` job, captioned "shared with job #N"), the probe file list, and the upload links **inline as each host completes** (parsed straight from `progress.hosts`, so they appear during the upload). Upload links render like Discord: plain clickable URL lines with no host prefix/left label; when the current upload payload contains only final URLs, the web hides the `100%` text. The web shows no separate "Links" section for upload jobs — only `uploaded_links` of non-upload jobs (e.g. backup_all `episodes`) get the `linksBlock`.

## Job construction

API jobs are built with `Job::new_api(...)` → `Frontend::Web`, so they run through the worker with no Discord context (see [WORKER.md](WORKER.md)).

## Web pages

All dependency-free, `include_str!`/`include_bytes!`-baked into `pndc`, same origin as `/api/v1` so no CORS; editing any requires rebuilding `pndc`):

- `GET /` → desktop shell (`web/desktop.html`)
- `GET /encode` → encode console (`web/index.html`)
- `GET /git` → git console (`web/git.html`)
- `GET /studio` → browser-native nonlinear Studio editor (`web/studio.html`)
- `GET /studio-sw.js` → the Studio editor's authenticated media-stream bridge
- `GET /favicon` (+ `/favicon.ico`) → site icon

The consoles fetch relative `/api/v1` paths. Details in `web/README.md`.

## Desktop shell

`web/desktop.html` (`GET /`): a small window manager over the consoles. A bottom **taskbar** has Encode/Git/Studio/**Jobs** launchers, a clock, an **API-token toggle button** (a popover whose password input writes the shared `localStorage` `pandora_token`), and the **☾/☀ theme toggle**. Each app opens as a draggable/**resizable** window whose body is an `<iframe>` to `/encode?embed=1` or `/git?embed=1`; windows have **traffic-light controls** (red = close, yellow = maximize, green = minimize), z-stacking on focus, and their open state + geometry persist in `localStorage` (`pandora_desktop_v1`). On mobile (≤760px) the WM is replaced by a launcher card linking to the standalone consoles. The desktop keeps its **own** copy of the `:root` theme vars for its chrome (a third place to retheme, alongside the two consoles).

## Embed & job-only modes

Consoles support:

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

`Dockerfile` (multi-stage — builds all workspace bins, runtime image bundles `ffmpeg`) + `docker-compose.yml` run `pndc` alongside a `cloudflared` sidecar on a shared network with **no published ports**; the Cloudflare tunnel's public-hostname service points at `http://pndc:8787` (the compose service name, not `localhost`). `DB/` is bind-mounted so the database, env, and tokens persist. See `web/README.md`.
