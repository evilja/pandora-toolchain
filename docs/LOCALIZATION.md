# LOCALIZATION.md

User-facing strings (status updates, embed fields, stage labels, job types, and common command-result labels) are language-aware.

- `src/pnworker/messages.rs` defines string IDs as `&'static str` consts (`pub const TORRENT_DONE: &str = "TORRENT_DONE";` etc.) — never as `usize` indices.
- `SERVER_EFFECTS_FAIL` is emitted when the post-download server-scoped subtitle effects step cannot probe the input or inject the configured watermark.
- `ENCODE_STALLED` (1 arg: minutes) is emitted by the worker loop, not by a worker: it is what a job says when the encode stall watchdog gives up on it (see [WORKER.md](WORKER.md#encode-stall-watchdog)).
- `MessagePayload` enum is what workers send over `CommData`:
  ```rust
  pub enum MessagePayload {
      Static(&'static str),
      Progress(&'static str, Vec<String>),
  }
  ```
  `Static` for terminal/fixed messages, `Progress` for templated messages with `{}` placeholders.
- `format_payload(&MessagePayload, &str) -> String` looks up the template, substitutes `{}` placeholders with `args`, and `eprintln!`s if fewer arguments arrive than the file's declared count (graceful — substitution still runs; preview payloads may carry extra attachment metadata).
- `get_message(id, lang) -> String` lowercases `lang` and reads `DB/config/<lang>.toml` (so server meta `EN` / `TR` / `JP` resolves to `en.toml` / `tr.toml` / `jp.toml`). Missing ids fall back to the matching built-in locale under `src/pnworker/locales/`; unknown languages fall back to English.
- `get_arg_count(id, lang) -> Option<usize>` reads the `args` field from the same runtime/built-in lookup.
- `format_message(id, lang, args)` exposes localized placeholder substitution to Discord command handlers as well as workers.
- `create_job_embed(job, &MessagePayload) -> CreateEmbed` formats the embed using `job.lang`. Job titles are selected from localized `JOB_TYPE_*` ids, so Probe/Backup/Preview/Studio jobs no longer say Encode. The embed localizes field labels and stages, keeps raw worker ids such as `dwl-pending` / `enc-main`, omits encode presets, never emits a blank source value, and only creates the Details field when the payload has non-status content. Nyaa sources are rendered as `/view/<id>` pages.
- `init_language_files()` seeds `en.toml` / `tr.toml` / `jp.toml` from the built-in locale files. On later starts it preserves custom values, merges newly introduced ids, and upgrades values that still exactly match Pandora's legacy generated table.
- Translation edits can be made live without restart because lookups read the TOML files on demand. `/touchtranslation` and `/gettranslation` edit/read one key; `/touchtranslationall` validates and replaces a full TOML attachment; `/gettranslationall` uploads the current TOML. These commands are Discord-only admin commands and are not exposed over the HTTP API.

### TOML format

```toml
[ENCODE_PROG]
text = "Geçiş `1/{}`\nKare `{} / {}` • `{} FPS` • `{} kbit/s`"
args = 5
```

One table per message ID. `text` is the template (use `\n` for newlines), `args` is the expected placeholder count.

### Adding a new message

1. Add the `&'static str` const to `messages.rs` with the same value as the name.
2. Add the same table and `args` count to `src/pnworker/locales/en.toml`, `tr.toml`, and `jp.toml`.
3. Send it from a worker as `MessagePayload::Static(NAME)` / `MessagePayload::Progress(NAME, vec![...])`, or use `format_message` from a command handler.
4. Run the locale key/argument-count test; startup merges the new id into existing runtime TOMLs without replacing custom translations.

The consts intentionally have the same name as the TOML keys, so `pub const X: &str = "X";` is the standard form. `src/pnworker/locales/legacy.toml` is migration data for the old generated table, not a fourth selectable language.
