# PROJECT.md

Guidance for coding agents working in this repository.

## Project layout

- `src/lib.rs` — crate root; re-exports modules and defines protocol macros (`pn_emit!`, `pn_schema!`, `pn_data!`, plus `lib_*` variants for in-crate use).
- `src/bin/` — binaries: `pndc` (Discord bot), `pncurl`, `pnp2p`, `pnmpeg`, `pnass`, `pnprotocol`, `pnkagami`.
- `src/libpncurl/` — HTTP layer: `core::Req` for downloads + multi-host uploads (Drive, Doodstream, Lulu, Voe, Abyss); `gscrape::GScrape` for Google Drive scraping.
- `src/libpnenv/` — env file loader (`get_env`, `add_env`, `get_perm`, `upsert_env`) + key constants in `standard.rs` (`CLIENT_ID`, `TOKEN`, `PNCURL`, `PNP2P`, etc.).
- `src/libpnlogging/` — `LoggingHandle` async logger + `log!` macro (takes `Option<LoggingHandle>`).
- `src/libpnp2p/` — torrent layer; `nyaaise::TorrentType` (`Link` / `Magnet` / `GDrive`) and `nyaaise()` URL classifier. `core::cleanup_pandora_qbit()` removes every qBittorrent torrent tagged `pandora-job-*` at worker startup.
- `src/libpnbin.rs` — startup/runtime binary bootstrap. `ensure_startup_binaries()` runs from `pndc` startup after config migration, validates tool paths, auto-fills sibling tool binaries into `env.pandora`, and installs portable `ffmpeg`/`ffprobe` into `DB/bin` when missing. `resolve_runtime_binary()` lets tools prefer `DB/bin/<name>` over PATH.
- `src/libpnmpeg/` — ffmpeg wrapper/progress parsing and ffprobe helpers; all `ffmpeg` / `ffprobe` process launches go through `libpnbin::resolve_runtime_binary`.
- `src/libpnprotocol/` — line-oriented stdout protocol (negotiation + tree-structured data); how tools talk to workers.
- `src/libpndb/` — sqlite job db (sqlx, WAL mode so the API can read while the worker writes). `JobRow` is the raw row; `JobStatus` is the API-facing serde DTO (`from_row`, plus `stage_label`/`job_type_label`/`preset_label`). `fail_stale_active()` (run once at `pn_worker` startup) marks every non-archived, non-terminal job `Failed` so a restart never shows phantom-active jobs; `get_active_jobs()` returns all non-archived rows, `get_ongoing_jobs()` only non-terminal ones (stage NOT IN 6/7/8/9). The `progress` and `uploaded_links` columns hold per-job JSON (set by `update_progress`/`update_links`); `server_id` persists the originating guild for API authorization; `JobStatus` parses `progress`/`links` JSON values and exposes `server_id` for the API.
- `src/libpnapi/` — axum HTTP API + self-served web console. `serve(tx, port)` is spawned from `pndc` when `api_port` is set and shares the worker's `Sender<JobClass>`, so API submits/cancels enter the same queue as Discord. Bearer-token auth (tokens in `api.pandora`); routes under `/api/v1` plus the baked web pages `GET /` (desktop shell), `/encode` (encode console), `/git` (git console), `/favicon`, and `/health`. See [API.md](API.md).
- `src/libkagami/` — ASS subtitle parsing/manipulation.
- `src/libpnmal/` — MyAnimeList client backed by JIKAN (`https://api.jikan.moe/v4`, no auth). `parse_mal_url`, `slugify`, `fetch_anime -> AnimeMeta { mal_id, kind: Movie|MultiEpisode, name, slug, episode_count, year, season }`. No env var needed.
- `src/libpnanisub/` — AniSub (`https://anisub.co`) REST client. `AniSub::new(api_key)` (Bearer auth); `search_anime`/`resolve_anilist` hit `GET /api/anime/search?q=` (no auth) and return `AnimeMatch { media_id (AniList id), title_turkish/english/japanese, url_slug }`; `upload_subtitle` POSTs multipart to `/api/admin/subtitle/upload` (`subtitle_file` zip, `subtitle_anilist_id`, `subtitle_release_name`, `subtitle_language="Türkçe"`, `subtitle_type="Çeviri"`, `content_type="Bölüm: NN"`, `fps` (`DEFAULT_FPS="23.976"`), `format="ass"`, `subtitle_translator`, `visibility="public"`). Uses `env[anisub]` (`ANISUB` const).
- `src/libpnforgejo/` — Forgejo REST client (`create_repo`, `list_contents`, `create_file`, `get_file_sha`, `update_file`, `upsert_file`) + inline base64 encoders `base64_encode` and `base64_encode_bytes`. Uses `env[FORGEJO_API_KEY]`. All contents endpoints share a `contents_url(host, owner_repo, path)` helper that uses `reqwest::Url::parse(...).join(...)` so paths with spaces are percent-encoded. `src/helpers/handlers/mod.rs` also defines a local `base64_decode_bytes` helper paired with `base64_encode_bytes`.
- `src/libpngit/` — frontend-agnostic Forgejo repo operations shared by the HTTP API: `init_repo`, `attach_repo`, `set_source`, `detach_channel`, `destruct_repo`, `smartcode_merge`, `list_attachments` (plus `Credits` and the `*Outcome`/`SmartMergeResult`/`Attachment` result structs). Also exports `README_BASE_GUIDE` (`pub const = include_str!("readme_guide.md")`) — the bundled README-template formatting guide, used as the final fallback for the git console's Credits/Readme view. `smartcode_merge` reuses `pnworker::util::run_tool` + the `PNASS_*` specs + `libkagami` and carries its own copy of the repo-ASS zip/base64 helpers. Mirrors the pndc `/init` `/attach` `/source` handlers but takes plain params (`server_id`, `channel_id`, `mal_url`, `season`, credits) and returns a result struct instead of editing Discord messages. It reads/writes `DB/config/<serverid>/<channelid>/meta.toml` and `DB/config/<serverid>/meta.pandora` directly (its own copy of `ChannelMeta`/bootstrap/`meta_to_toml`, identical to the binary's so both paths produce the same files). The Discord handlers in `src/bin/pndc.rs` are **not** wired to this module — they keep their own copy, so the two must stay in sync if the meta format changes.
- `src/helpers/` — pndc-only helper modules included by `src/bin/pndc.rs`: `pndc.rs` contains command option parsing, response helpers, attached-repo validation, and Forgejo config loading; `handlers/mod.rs` re-exports Discord command handlers (`handle_*`) split across `src/helpers/handlers/*.rs` plus shared handler-local helpers.
- `src/pnworker/` — worker runtime used by `pndc`. `core.rs` runs the main loop and `pn_worker()`; `frontend.rs` defines the `Frontend` enum (`Discord { ctx, msg }` / `Web` / `None`) that decouples `Job` from serenity — every message edit, reaction, and presence update is routed through it, with `Web`/`None` as no-ops; `messages.rs` is the localization gate (consts, get_message, format_payload, create_job_embed); `workers/` contains `downloadworker`, `encodeworker`, `uploadworker`, `probeworker`; `tools.rs` declares CLI specs for each tool (`PNCURL_*`, `PNP2P_*`, `PNMPEG_*`, plus `PNASS_LAYER`, `PNASS_SPLIT_SIGNS`, `PNASS_MERGE`, `PNASS_MERGE_TL_ONLY`); `util.rs` has `run_tool` (spawns a tool and dispatches its protocol lines to a callback), `WorkerNamePool` (randomly assigns/reclaims per-task names), and `IntrosConfig` (loads `DB/config/global/environment/intros.toml`); `heartbeat/` is the `TypedShrine` supervisor (auto-reboots dead workers); `presence.rs` owns the `Presence` enum and `change_presence_job` / `presence_from_queue` helpers used by `core.rs` to update the Discord activity status and presence. See [WORKER.md](WORKER.md).

## Build / verify

- Build: `cargo build` (full workspace) or `cargo build --bin <name>`.
- Lint check: `cargo check --all-targets`.
- Tests: `cargo test --lib` (mostly in `libpnp2p::nyaaise::tests`).
- No formatter / clippy config is enforced; match surrounding style.

After any change, run `cargo check --all-targets` at minimum.

## Conventions

- **No comments** unless explicitly requested. Existing reference URL comments in files are fine to preserve; do not add new ones.
- **No doc comments either** — the codebase does not use `///`.
- **Error handling is loose**: many places use `.unwrap()` on regex/IO/json that are expected to succeed; match that style. `Result<(), Box<dyn std::error::Error>>` is the common shape; `Send + Sync` is only added when the value crosses a spawn boundary (see `get_access_token` in `libpncurl/core.rs`).
- **Async**: `tokio` everywhere. Use `tokio::fs` and `tokio::io::AsyncWriteExt` for new IO; use reqwest client timeouts where appropriate, but `pncurl` uploads intentionally have no request deadline (only connect timeout). Streaming downloads use `resp.chunk().await?`.
- **Macros**: use `Regex::new(r"...").unwrap()` per-call (no `lazy_static`/`once_cell` — neither is a dep). Logging is `log!(handle_opt, "msg\n")`.

Worker-specific patterns live in [WORKER.md](WORKER.md).

## Environment / runtime

- `env.pandora`: key-value config file. Each line is `NAME|pntools|VALUE` (literal `|pntools|` separator). Key names are the `&'static str` consts in `libpnenv/standard.rs` (e.g. `discord_token`, `pnass`, `pnmpeg`). `get_env` returns a `HashMap<String, String>`; missing keys produce empty values via `.unwrap_or_default()`. `migrate_env_format` (called from `migrate_pandora_files` at startup) detects the old line-indexed format (no line contains `|pntools|`) and rewrites it to the new key-value format. Not committed.
- `DB/config/global/environment/intros.toml`: optional intro candidates for the encoder. Not committed.
- `DB/config/global/base.md`: optional operator-wide README-template guide; served as the Credits/Readme fallback when a server has no `DB/config/<serverid>/base.md`, before the binary-bundled `libpngit::README_BASE_GUIDE`. Not committed.
- `DB/config/global/favicon.{png,ico,svg,jpg,jpeg,webp,gif}`: optional favicon override for `GET /favicon` (otherwise the bundled `web/favicon.png` is served). Not committed.
- `DB/bin/`: runtime-managed portable binaries. On `pndc` startup, `libpnbin::ensure_startup_binaries()` creates this directory, checks `ffmpeg` and `ffprobe` in PATH first, then checks `DB/bin`, and if both are unavailable downloads a portable FFmpeg build. Supported downloads: Linux `x86_64`/`aarch64`/`arm` from John Van Sickle static `.tar.xz` builds, and Windows `x86_64` from Gyan `ffmpeg-release-essentials.zip`. Linux extraction shells out to `tar -xJf`; Windows zip extraction uses `async_zip`. Installed binaries are `DB/bin/ffmpeg` / `DB/bin/ffprobe` or `.exe` variants. `pnmpeg` uses `resolve_runtime_binary()` so `DB/bin` is preferred when present.
- Startup also validates `env.pandora` keys `pnmpeg`, `pnp2p`, `pncurl`, and `pnass`: if the configured value is missing/unusable, it looks for matching tool binaries next to the running `pndc` executable (then in `DB/bin`) and writes the discovered path with `upsert_env`. It only auto-downloads `ffmpeg`/`ffprobe`; the Pandora tool binaries themselves are expected to be built/distributed with `pndc`.
- **`DB/config/<serverid>/meta.pandora`** — server-scoped config, line-indexed:
  - line 0: language code (`EN` / `TR` / `JP`)
  - line 1: Forgejo org link (full URL like `https://git.einzu.fun/AkiraSubs`, trailing `/` stripped at write time). `/init` uses the last path segment as the org; `/attach` ignores it.
  - line 2: announcement channel id (captured implicitly by `/configure` from `command.channel_id`)
  - line 3: Forgejo API key
  - line 4: per-guild Google Drive client id (optional; falls back to global env when all Drive fields are empty)
  - line 5: per-guild Google Drive client secret
  - line 6: per-guild Google Drive refresh token
  - line 7: per-guild Google Drive folder id
  - line 8: ASS WrapStyle normalization (`""`/missing/`dont_touch` means preserve existing WrapStyle; `0`/`1`/`2`/`3` forces that value). `/configure` and `/edit` expose this as `wrapstyle`; default is `dont_touch`.
- **`DB/config/<serverid>/<channelid>/meta.toml`** — per-channel anime attachment (written by `/init` and `/attach`; removed by `/detach`, and **auto-removed when the Discord channel/thread is deleted** — `pndc`'s `channel_delete`/`thread_delete` handlers call `auto_detach_channel`, which deletes the meta like `/detach` and leaves the repo untouched):
  - `mal_id`, `kind` (`Movie` | `MultiEpisode`), `name`, `slug`, `episode_count`, `repo_url`
  - `episode_count_at_git` (count of `pad2(n)` episode folders already in the Forgejo repo at attach time)
  - `year` (optional; from JIKAN `data.year` or the first 4 chars of `data.aired.from`)
  - `season` (1-based sequel number; defaults to 1, set via the optional `season` option on `/init` and `/attach`)
- **`DB/config/<serverid>/channels.json`** — a published snapshot of the guild's selectable Discord channels (`[{ id (string), name, kind }]`, kind ∈ Text/Announcement/Forum/Thread/…), written by `pndc`'s `sync_guild_channels` on `cache_ready`/`guild_create` and re-synced on channel/thread create/update/delete. Not authoritative — it's a convenience cache so the HTTP API (`GET /git/channels`) and the web git console's Init/Attach pickers can list channels without a Discord handle. Not committed (under gitignored `DB/`).
- **`DB/config/<lang>.toml`** — localized message tables (`en.toml`, `tr.toml`, `jp.toml`); see [LOCALIZATION.md](LOCALIZATION.md).
- Working directory at runtime: `DB/work/<job_id>` for in-flight jobs, `DB/saved_data/<job_id>` for archived job artifacts (this is also where `/job` writes its per-call `input.ass` / `output.ass` / `extract/`). The worker also uses `DB/cache/inputs/<md5(torrent|get-index)>/input.mkv` for a 30-minute encode-input cache; `touch` in that directory resets the TTL, and `pn_worker` deletes the whole input cache on startup. The whole `DB/` tree is gitignored.

Server-side auth and command tiers are documented in [DISCORD.md](DISCORD.md). HTTP API config and tokens are documented in [API.md](API.md).
