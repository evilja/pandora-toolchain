# LOCALIZATION.md

User-facing strings (status updates, embed fields, stage labels, preset labels) are language-aware.

- `src/pnworker/messages.rs` defines string IDs as `&'static str` consts (`pub const TORRENT_DONE: &str = "TORRENT_DONE";` etc.) — never as `usize` indices.
- `MessagePayload` enum is what workers send over `CommData`:
  ```rust
  pub enum MessagePayload {
      Static(&'static str),
      Progress(&'static str, Vec<String>),
  }
  ```
  `Static` for terminal/fixed messages, `Progress` for templated messages with `{}` placeholders.
- `format_payload(&MessagePayload, &str) -> String` looks up the template, substitutes `{}` placeholders with `args`, and `eprintln!`s if `args.len()` doesn't match the file's declared count (graceful — substitution still runs).
- `get_message(id, lang) -> String` lowercases `lang` and reads `DB/config/<lang>.toml` (so server meta `EN` / `TR` / `JP` resolves to `en.toml` / `tr.toml` / `jp.toml`). It falls back to the inlined `DEFAULT_ENTRIES` in `messages.rs` if the file is missing or the id is unknown; falls back to `""` if neither is present.
- `get_arg_count(id, lang) -> Option<usize>` reads the `args` field from the same TOML.
- `create_job_embed(job, &MessagePayload) -> CreateEmbed` formats the embed using `job.lang`. It looks up every field label, stage name, and preset label through `get_message`. The embed title uses `{}` substitution (e.g. `"Encode İşlemi ({})"`), and the embed shows the translatable `FIELD_WORKER` header with the raw internal `job.worker` value (do not translate worker ids such as `dwl-pending` / `enc-main`).
- `init_language_files()` writes `en.toml` / `tr.toml` / `jp.toml` from `DEFAULT_ENTRIES` at bot startup, but only if the file doesn't exist. So manual edits to those files are preserved across restarts.
- Translation edits can be made live without restart because lookups read the TOML files on demand. `/touchtranslation` and `/gettranslation` edit/read one key; `/touchtranslationall` validates and replaces a full TOML attachment; `/gettranslationall` uploads the current TOML. These commands are Discord-only admin commands and are not exposed over the HTTP API.

### TOML format

```toml
[ENCODE_PROG]
text = "\n\nDosya encode ediliyor.\nAşama: 1/{}\nİşlenen kare: {}/{}\nSaniye başına işlenen kare: {}\nSaniye başına ortalama veri: {}kbit/s"
args = 5
```

One table per message ID. `text` is the template (use `\n` for newlines), `args` is the expected placeholder count.

### Adding a new message

1. Add the `&'static str` const to `messages.rs` with the same value as the name.
2. Add a `(name, text, args)` tuple to `DEFAULT_ENTRIES`.
3. Send it from the worker as `MessagePayload::Static(NAME)` or `MessagePayload::Progress(NAME, vec![...])`.
4. To translate, add the same table to `en.toml` / `tr.toml` / `jp.toml`. They'll be auto-seeded on the next startup if missing.

The consts intentionally have the same name as the TOML keys, so `pub const X: &str = "X";` is the standard form.
