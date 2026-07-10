# DISCORD.md

Discord-facing behavior: commands, authorization tiers, presence updates, and the in-handler `/job` and `/smartcode` flows.

## Authorization allowlists

Authorization is managed in `bin/pndc.rs` (one Discord user-id per line):

- `authorize.pandora` — `/encode`, `/probe`, `/pancode`, `/backup`, `/gitcode`, `/smartcode`, `/source`
- `upper.pandora` — `/attach`, `/init`, `/gentoken`, `/destruct`, `/detach` (privileged workflow)
- `fansubber.pandora` — `/job` (subtitle-uploader workflow, kept separate from repo-`/init` so a translator/typesetter can be granted the lighter tier without repo-creation rights)
- `admin.pandora` — `/hearts`, `/gitsync`, `/gitquery`, `/configure`, `/edit`, `/touchapi`, `/gettranslation`, `/touchtranslation`, `/gettranslationall`, `/touchtranslationall`, `!auth`, `!ban`
- `witch.pandora` — `/acixconfirm`, `/akiraconfirm`, `/acixtemplate`, `/touchflavor`, `/lsflavor`, `/rmflavor`, `/touchpool`, `/lspool`, `/rmpool`, `/changerank`, `/fontcheck`, `/touchintro`

The level hierarchy is `witch > upper > admin > fansubber > authorize` (rank 4/3/2/1/0). A user is considered to be at "rank R" if they appear in any of `witch.pandora` (R=4), `upper.pandora` (R=3), `admin.pandora` (R=2), `fansubber.pandora` (R=1), or `authorize.pandora` (R=0). `is_authorized(part, id)` consults `min_rank_for_command(part)` and `has_level_at_least(id, min_rank)` — so a higher-ranked user can run any command whose minimum rank is ≤ their own (e.g. an `upper`-tier user can run `/gitsync` because `3 >= 2`). `/help` and `/providers` bypass the allowlists and are visible to everyone. `/auth` and `/rm` additionally verify `has_level_at_least(caller_id, level_rank(target_level))` inside the handler, so a user can only grant/revoke tiers they outrank or equal — an admin can auth `authorize`/`fansubber`/`admin` but not `upper`, and a fansubber-tier user cannot run `/auth` at all.

## Discord commands

- `/help [section]` — public, ephemeral command guide. Bare `/help` shows section overview; `section` choices are `encode`, `repo`, `workers`, `admin`, `publish`, `fonts`, and `misc`. Section and command menus are filtered to commands the caller can run.
- `/encode <torrent> <subtitle attachment> [preset] [concat]` — encode with an attached ASS file. Accepts torrent URLs, magnet links, Google Drive links, and direct video file links.
- `/providers` — public command that shows built-in download/encode support and currently attached provider APIs: upload providers from env/global+server Drive config (Google Drive, Doodstream, LuluStream, Voe, Abyss), distribution providers (AnimeciX, AniSub), and persistence providers inferred from the server Forgejo/GitHub org config. Implemented in `src/helpers/handlers/providers.rs` and available to everyone like `/help`.
- `/probe <torrent>` — download + ffprobe a torrent, list the files inside as a numbered table, then idle at `Probed` for 180s so a follow-up `/pancode` can pick a file. GDrive and direct video links are rejected.
- `/pancode <job_id> <index> <subtitle attachment> [preset] [concat]` — re-encode using a previously probed torrent's `fetch.torrent` (the probe job's `contents/fetch.torrent` is copied into the new job's dir). When this finishes, the parent probe job is archived.
- `/backup <torrent>` — download + Drive-only re-upload (no streaming hosts). GDrive and direct video links are supported (treated as downloads from non-torrent sources).
- `/gitcode <torrent> <subtitle_url> [preset] [concat]` — like `/encode` but the subtitle is fetched from a URL. `https://github.com/<u>/<r>/blob/<b>/<path>` is auto-rewritten to `https://raw.githubusercontent.com/<u>/<r>/<b>/<path>`; other URLs pass through. 60s HTTP timeout.
- `/smartcode run <episode> [link] [preset] [concat]` — merge the channel's attached TL (required) and TS (optional) subtitles for an episode via `pnass --merge`, upload the merged result to the channel's repo as `Release - <name> - E<NN>.ass`, upsert `SOURCE.md`, then queue a regular `/encode` job against the merged file. `link` is optional: if absent, the source link is read from `{pad2(episode)}/SOURCE.md` (parser skips blank/`;`-prefixed lines and strips a leading `#`); the existing `SOURCE.md` is left untouched in that case. See [`/smartcode`](#smartcode) for the merge details.
- `/smartcode exp <episode> [link]` — runs the same smartcode merge/upload step, then renders 1-3 TS preview screenshots from `\fn` typeset lines instead of encoding.
- `/source <episode> <link>` — write `{pad2(episode)}/SOURCE.md` (content `# <link>\n`) to the channel's attached Forgejo repo. Requires the channel to be attached and `episode` in `1..=episode_count`. Commit message: `"Set source link"`. No worker, no encoder — pure in-handler Forgejo upsert.
- `/attach <mal> <repo> [season]` — fetch MAL metadata via JIKAN, then bootstrap an existing Forgejo repo: create per-episode folders (`pad2` for 1..=episode_count, accepting `1`/`01`/`001` as equivalent on existence check), each with an empty `.gitkeep`; create `README.md` at root only if absent (and only if `DB/config/<serverid>/base.md` is present). Requires both `mal` and `repo`. `season` is the 1-based sequel number stored in the channel meta (defaults to 1). Repos are public.
- `/init <mal> [season]` — same bootstrap, but creates a new public repo at `<forgejo_org>/<slug>` via the Forgejo API first. `season` works the same as `/attach`. Channel reattach to a different MAL id is refused; same MAL id is idempotent.
- `/detach` — **upper-tier**; removes the channel's `meta.toml` attachment; the Forgejo repo is left untouched. In-handler, no worker. (Also happens automatically when the channel/thread is deleted — see the `meta.toml` note in [PROJECT.md](PROJECT.md).)
- `/destruct` — **upper-tier**; deletes the channel's Forgejo repo (`delete_repo`) **and** removes the attachment. Irreversible. In-handler, no worker.
- `/hearts` — admin; reports each shrine layer's `alive` / `last_beat_secs` / `reboot_count`.
- `/workers` — admin; shows a Discord embed diagram with download, core (configured `prb-*` slots plus `enc-main`), and upload columns, plus active-job details and queued/cache-forward waiting work.
- `/touchworker <type> <name>` / `/lsworker` / `/rmworker <type> <name>` — witch; add/list/remove configurable download, probe, or upload worker slots. Running pools refresh this config automatically; removed active slots finish their current job first.
- `/gitsync` — admin; `git fetch` + fast-forward, kills the shrine, archives `DB/work`, `std::process::exit(0)` to restart.
- `/gitquery` — admin; disables new encode jobs, waits for current encode jobs to finish, then runs the same sync/restart path as `/gitsync`.
- `/configure <language> [forgejo] [wrapstyle]` — admin; writes `DB/config/<guild_id>/meta.pandora` and records the channel the command was issued in as the announcement channel. `language` is `EN` / `TR` / `JP` (string choice). `forgejo` is optional — leave empty to unset. `wrapstyle` controls ASS WrapStyle normalization (`dont_touch` default, or `0`/`1`/`2`/`3`). `/edit` can update the same field without rewriting the rest of the config.
- `/gettranslation <language> <key>` — admin; reads one localization entry from `DB/config/<language>.toml` (`language` choices are `en` / `tr` / `jp`) and replies ephemerally with its text and `args` count. Handler: `src/helpers/handlers/translation.rs`.
- `/touchtranslation <language> <key> <text> [args]` — admin; upserts one localization entry in the selected TOML. Existing keys keep their current `args` count unless `args` is provided; new keys infer `args` from `{}` placeholders when omitted.
- `/gettranslationall <language>` — admin; replies ephemerally with the full selected language TOML as an attachment.
- `/touchtranslationall <language> <file>` — admin; replaces the selected language TOML from an uploaded `.toml` attachment after UTF-8 and TOML parse validation. Empty TOML maps are rejected.
- `/job <type> <episode> <subtitle> [commit]` — submit a single-episode job against the channel's attached anime; handled in-handler by `pndc`, no worker. See [`/job`](#job) below.
- `/cfont [font]` — set or show this server's `/smartcode exp` preview watermark font. The default requested face is `Gandhi Sans Bold`; install it with `/font` if that exact font is desired. If the configured/default font cannot be resolved, preview rendering falls back to the embedded Liberation Mono font.
- `/gentoken [label] [local]` — **upper-tier**; mints a 64-hex-char API bearer token (cross-platform CSPRNG via `getrandom`, so it works on the Windows VDS), appends it to `api.pandora` (with an optional `; <label> (added <ts>)` comment line above it), and replies ephemerally with the token shown once. With `local: true` the line is written as `<token>|local|<guild_id>`, binding the token to the issuing server (uses its Drive creds for uploads and **unlocks the git console / git endpoints**). Handled in-handler (`src/helpers/handlers/gentoken.rs`), no worker. See [API.md](API.md).
- `/acixconfirm <job_id>` — **rank 4 (Witch tier)**; confirms a finished uploaded job and publishes the multishare links to AnimeciX.
- `/akiraconfirm <job_id> <episode> <name> [slug] [folder]` — **rank 4 (Witch tier)**; creates the Akira episode if missing, or updates only the existing episode's link fields if it already exists, then replaces its episode links with the job's uploaded links. If the channel has a MAL id, the Akira resolver supplies the current official slug; otherwise `slug` falls back to the command option or attached channel slug. The Drive upload is published as an Akira index player URL under `folder` (defaults to the resolved slug), using `name` as the index filename.
- `/touchflavor <text>`, `/lsflavor [page]`, `/rmflavor <index>` — **rank 4 (Witch tier)**; manage global idle presence flavor lines in `DB/config/global/environment/flavor.pandora`. When the queue is empty, presence randomly picks one stored line instead of `No jobs in queue.`; if none exist, the default text remains.
- `!auth <user_id>` / `!authorize <user_id>` — admin; appends a user id to `authorize.pandora`.
- `!enc` — replies pointing the user at `/encode`. Legacy.
- ❌ reaction on a job's response message — emits a `HalfJob(Cancel)` that touches a `CANCEL` sentinel file in the job's working directory; the worker process picks it up via `cancelfile` polling. (`/job` does not participate — it's in-handler, atomic.)

Torrent classification, duplicate handling, and cache behavior are covered in [WORKER.md](WORKER.md).

## Discord presence

`src/pnworker/presence.rs` drives the bot's Discord activity + status from the in-memory job queue.

- `Presence` enum: `Idle`, `QueueTotal(usize)`, `Downloading { idx, total }`, `Encoding { idx, total }`, `Uploading { idx, total }`, `Probing { idx, total }`.
- Presence updates are **routed through `Frontend::set_presence(presence)`**, not called directly — for a `Discord` job it invokes `change_presence_job`, and for `Web`/`None` it's a no-op (an API-only job never touches the Discord activity). `change_presence_job(ctx, presence)` (in `presence.rs`) sends the status: active stages use `OnlineStatus::DoNotDisturb`; `Idle` and `QueueTotal` use `Online`. It's `async` for caller uniformity, but `serenity::all::Context::set_presence` is **not** a future in this version — never `.await` on the inner call.
- `presence_from_queue(&[Job]) -> Presence`: scans the queue and picks a single representative stage with priority `Uploading > Encoding > Downloading > Probing > Probed`. Falls back to `QueueTotal(queue.len())` when no active job exists. `Idle` / `QueueTotal(0)` read `flavor.pandora` and randomly use one non-empty line when present.

`core.rs` updates the presence at every stage transition via `job.frontend.set_presence(...)`:

- Encode/Pancode/Backup dispatch → `Downloading { idx: queue.len(), total: queue.len() + 1 }` (no `if queue.len() == 1` guard).
- Probe dispatch → `Probing { idx, total }`.
- `Downloaded → Encoding` → `Encoding { idx, total: qlen }`.
- `Encoded → Uploading` and Backup's `Downloaded → Uploading` → `Uploading { idx, total: qlen }`.
- Unified finish block: when a job reaches a terminal stage it captures `finished_fe = Some(i.frontend.clone())` before the `queue.retain`/archive; after the loop, `if let Some(fe) = finished_fe { fe.set_presence(presence_from_queue(&queue)).await }`. Cloning the `Frontend` (instead of holding a `&mut queue` borrow) is what lets the presence be recomputed against the already-shrunk queue.
- Probe timeout (`Stage::Probed` after 180s) → clones `queue[pos].frontend`, removes the timed-out job, then calls `frontend.set_presence(presence_from_queue(&queue)).await`.

## `/job`

Slash command `type` (TL / TLC / TS, required) + `episode` (1-based int, required) + `subtitle` (attachment, required) + `commit` (optional string). Runs entirely in `src/helpers/handlers/job.rs` — no worker, no `JobType`, no `❌` cancel path. The channel **must** already be attached (`read_channel_meta` non-empty) and the episode must be in `1..=episode_count`.

Flow:

1. Download the attachment. `.ass` → write straight to `DB/saved_data/<response_msg_id>/input.ass`. `.zip` → extract via `async_zip` over a temp file; walk root-level entries (no recursion), collect paths ending in `.ass` case-insensitively. Exactly one → move to `input.ass`. Zero or more than one → reply with an error. Anything else → `unsupported subtitle file type`.
2. Standardise only the ASS `[Script Info]` header into `output.ass`: set `Title:` to `<Org> - <Anime Name>` (or just `<Org>` if the JIKAN name is empty), fill the standard header keys, preserve existing `PlayResX/Y` when present, and only write `WrapStyle:` when the server's line-8 wrapstyle config is `0`/`1`/`2`/`3`. It does **not** invoke pnass and does **not** touch event layers or parsed event/style data.
3. Read `output.ass`, base64-encode the **bytes** (`base64_encode_bytes`), compute:
   - `folder = pad2(episode)`.
   - `file_type_label = "TL"` for `TL` and `TLC`, `"TS"` for `TS` — **TLC edits the TL file**, so its target filename is the same as `TL`'s.
   - `name = meta.name` with `/` replaced by `-` (filesystem-safe).
   - `file_name = "{file_type_label} - {name} - S{season:02}E{episode:02}.ass"`, where `season` comes from the channel meta.
   - `repo_path = "{folder}/{file_name}"`.
4. Commit message:
   - Default by type: `TL` → `"Translation"`, `TLC` → `"Edit"`, `TS` → `"Typeset"`.
   - If the user supplied a non-empty `commit`, the prefix `[TL]` / `[TLC]` / `[TS]` is prepended (`"[TLC] review pass"`).
5. Upload via `fg.upsert_file(&owner_repo, &repo_path, &b64, &commit_msg)`. The upsert transparently handles "file already exists" by reading the existing sha and PUTting.
6. Edit the response with an embed (`EditMessage::new().content("").embed(...)`):
   - **Repo** (inline) — `owner_repo`.
   - **File** (inline) — `repo_path`.
   - **Job** (inline) — `job_id` (the response message id).
   - **Commit Message** (block) — `commit_msg`.
   - **Warnings** (block) — `format_warnings_field(&warnings)`: `"None"` if empty, otherwise a bullet list of `<event_number>: <visible line>` and `<N> more similar warnings` entries, truncated to the 1024-char Discord embed field limit with a `…and N more` tail.

`/job` intentionally does not run `PNASS_LAYER`; it is a repository upload/header-standardisation path only. The Warnings embed field is currently normally `None`.

## `/smartcode`

Slash command with two subcommands:

- `/smartcode run episode:<n> [link] [preset] [concat]` merges the channel's attached TL and TS subtitles, uploads the result, and queues a regular `JobType::Encode` against the merged file.
- `/smartcode exp episode:<n> [link]` runs the same merge/upload flow, then queues `JobType::Preview` to render 1-3 screenshot previews from TS `Dialogue` events containing `\fn` font override tags.

The channel **must** already be attached (`read_channel_meta` non-empty) and the episode must be in `1..=episode_count`.

Flow:

1. Resolve `link`:
   - If the user supplied a non-empty `link` argument, use it directly.
   - Otherwise, fetch `{pad2(episode)}/SOURCE.md` from the attached repo via `fg.get_file_content` and base64-decode (`base64_decode_bytes`). Parse: skip blank lines and `;`-prefixed comments; take the first non-empty line; strip a leading `# `; trim. Missing file → bail with an error.
2. Classify the resolved link with `nyaaise(&link)` to pick a `TorrentType`.
3. Download TL (required) and TS (optional) from `{pad2(episode)}/TL - {safe_name} - E{pad2}.ass` and `{pad2(episode)}/TS - {safe_name} - E{pad2}.ass` via `fg.get_file_content`. Stash them in a per-call temp dir (`temp_dir/pandora_smartcode_{nanos|job_id}/`). If TS is absent, run `PNASS_SPLIT_SIGNS` first: TL events whose style name contains `Sign` are moved, with their used styles, into a generated TS file; TL is updated without those sign events; both files are uploaded back to the repo and a warning/notification is shown.
4. Run `pnass --merge` (the `PNASS_MERGE` spec when TS is present, `PNASS_MERGE_TL_ONLY` when it isn't) via `pnworker::util::run_tool`. The pnass negkey for this flow is `PNassMerge` (separate from the `PNass` one used by `PNASS_LAYER`), so the tool can detect it's being driven by smartcode. The merge specs pass `--smart-layer 9` and `--wrap-style <server setting>`; only events with no override tags beyond basic bold/italic/underline/strikeout get layer-normalised, and sign-style events keep their existing layer. Output goes to `output.ass` in the same temp dir. Non-`ToolResult::Success` → reply with `"Merge failed: <err>"` and bail.
5. Upload the merged ASS as `Release - {safe_name} - E{pad2}.ass` via `fg.upsert_file`, commit message `"Smartcode merge"`.
6. Resolve the source-link origin:
   - If `link` was supplied as an argument → write `SOURCE.md` with `# {link}\n` (commit `"Smartcode source"`).
   - If `link` was read from `SOURCE.md` itself → skip the rewrite (the file already contains it).
7. For `run`, build a `JobType::Encode` from the merged bytes + the resolved source link, the same way `/encode` would, and submit it to the worker queue via `self.tx.send(JobClass::Job(job))`. Named local smartcode Drive uploads store the returned file ID and Drive folder ID under the channel config; when a later named smartcode upload for the same episode completes, the previous stored Drive file is deleted and the state is replaced with the new IDs.
8. For `exp`, parse TL and optional TS with structured ASS parsing. Any actor/effect `stamp` comments supply the first three manual frames; otherwise timed TS dialogue is grouped into 1-second-gap clusters and ranked by `\fn` presence, drawings, line count, tag count, duration, and start time. Selected shots keep a hard 10-second gap and prefer clusters outside the post-shot cooldown before backfilling. The optional `cooldown` argument is measured in seconds, defaults to 90, accepts 0 through 3600, and uses 0 to disable cooldown. The overlay shows timestamp, cluster/stamp length, and normalized rank weight (`+` for stamps); `preview_ranking.log` is archived with the job logs.

Preview watermark font is configured per server with `/cfont [font]`, stored at `DB/config/<server_id>/preview.toml` as `watermark_font`. The default requested font is `Gandhi Sans Bold`; the bot does not ship it, so install it with `/font` if needed. Rendering falls back to the embedded Liberation Mono font when no configured/default font resolves.

The merge summary is printed to the bot's stdout (e.g. `smartcode merge: <TL> + <TS> -> <output> for episode <N>, source_origin=argument|SOURCE.md`); the per-stage progress is reflected via the normal Discord presence updates of the queued worker job.

### pnass `--merge` semantics

- `--input <path>` (TL) and `--output <path>` are required. `--merge <path>` is the optional secondary ASS (TS). When absent, the merge step is skipped and `--input` is copied to `--output` after the configured smart-layer pass.
- Style name disambiguation: the intersection of TL and TS `Style` names is computed. If non-empty, the overlapping style names in the **secondary** (TS) file are renamed to `pn-<random10>` (lowercase a–z + 0–9, 10 chars; xorshift seeded from `SystemTime::UNIX_EPOCH`). TL's style names are never touched, so the merged file preserves the original TL style names.
- Event append: TL's events are emitted first, then TS's events are appended. Same for styles (TL styles first, then the renamed TS styles).
- Drawing-mode events in the secondary are kept as-is. Override blocks are honored (the secondary is loaded with `adv_parsing=true`).

See [TOOLS.md](TOOLS.md) for full `pnass` flags and ASS parsing rules.
