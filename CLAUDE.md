# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Docs first

`docs/` is the authoritative, maintained description of this project — read the relevant file before changing an area, and update it when behavior changes:

- **docs/PROJECT.md** — module-by-module layout, coding conventions, and every on-disk config/runtime file format (`env.pandora`, `DB/config/<serverid>/meta.pandora` line indices, `meta.toml`, caches).
- **docs/DISCORD.md** — commands, authorization tiers, presence, `/job` and `/smartcode` flows.
- **docs/WORKER.md** — worker runtime, tool orchestration, torrent routing, cache/duplicate behavior.
- **docs/API.md** — HTTP routes, bearer tokens, web consoles, deployment.
- **docs/TOOLS.md** — `pncurl`, `pnass`, libkagami parsing, ASS line-length checks.
- **docs/LUMIERE_BROKER.md** — Cloudflare Worker, Drive profiles, secret migration, upload data flow.
- **docs/LOCALIZATION.md** — message IDs and translation TOMLs.

`AGENTS.md` is the same index for other agents; keep the two consistent if you change the doc set.

## Build / test

```bash
cargo check --all-targets          # run this at minimum after any change
cargo build                        # all binaries; --bin <name> for one
cargo test --lib                   # ~275 unit tests, seconds to run
cargo test --lib <filter>          # single test or module, e.g. nyaaise::tests
cargo test -p kagami-trace         # the sub-crate's own tests
./start.sh                         # clean + build + run pndc in a restart loop
```

No formatter or clippy config is enforced; match surrounding style. Tests are `#[cfg(test)] mod tests` blocks inline across `src/lib/**`, `src/pnworker/**`, and `src/lumiere-broker/**`.

Docker (`docker-compose.yml`) builds release binaries and runs `pndc` behind cloudflared; `DB/` is a mounted volume.

## Architecture

**One crate, many binaries.** `src/bin/*.rs` are separate processes: `pndc` (Discord bot + worker runtime + HTTP API, the only long-running one), and the tools `pncurl`, `pnp2p`, `pnmpeg`, `pnass`, `pnkagami`, `pnprotocol`, `pntrace`. `kagami-trace/` is a path dependency that must stay extraction-ready — it never depends back on Pandora (`pntrace` and `src/libkagami/tracing.rs` hold the Pandora-side glue).

**Non-obvious module wiring.** `src/lib.rs` maps directories with `#[path]` (`lib/mod.rs`, `lumiere-broker/mod.rs`). `src/helpers/` is *not* part of the library — `src/bin/pndc.rs` pulls it in with `#[path = "../helpers/pndc.rs"]`, and `helpers/handlers/mod.rs` starts with `use super::*`, so handlers see the binary's imports. Adding a handler means creating `src/helpers/handlers/<name>.rs` and declaring `mod <name>;` there.

**Tools talk to workers over stdout, not function calls.** `src/lib/protocol/` defines a line-oriented negotiated protocol; `src/lib.rs` exposes the `pn_emit!` / `pn_schema!` / `pn_data!` macros tools use to emit it (`lib_*` variants are for in-crate callers). A worker declares a `CliParam` spec in `pnworker/tools.rs` (`PNCURL_*`, `PNP2P_*`, `PNMPEG_*`, `PNASS_*`), spawns the tool with `pnworker::util::run_tool`, and dispatches opcodes in a closure: `0` progress, `1` success, `2` fail, `3` cancel, `4` custom, `5` duplicate torrent. Changing a tool's CLI or emitted schema means changing both sides.

**One job queue, two frontends.** `pndc::main` creates a `channel(5)` of `JobClass` and spawns both the worker loop (`pnworker::core::pn_worker`) and, when `api_port` is set, `lib::http::api::serve` with a clone of the sender — Discord and HTTP submits land in the same pipeline. `Job` never touches serenity directly: all user-visible effects go through `pnworker::frontend::Frontend` (`Discord` / `Web` / `None`), so web-submitted jobs no-op the message edits.

**All user-facing strings are localized.** Nothing hardcodes English into worker/handler output — add a const + entry in `src/pnworker/locales/{en,tr,jp}.toml` and fetch via `pnworker::messages`. A unit test enforces matching keys and arg counts across locales.

**Provider clients are thin adapters.** The external `capella` crate owns HTTP for AniSub, AnimeciX, OpenAnime, Anizm, and Hyperkira; `src/lib/http/<provider>/` adds only Pandora concerns (env/session persistence, caches for autocomplete, fansub resolution). Uploads go through `src/lumiere-broker/` (secretless, credentials live in Cloudflare Worker secrets) — not through `lib/http/curl`, which is the legacy/`pncurl --drive` path.

**Two copies of the git logic.** `src/lib/git/` reimplements `/init` `/attach` `/source` `/detach` for the HTTP API with plain params, while `src/bin/pndc.rs` keeps its own copy for Discord. They read and write the same `meta.toml` / `meta.pandora` files, so a change to either the meta format or the flow must be applied to both.

**Config formats are positional and unforgiving.** `env.pandora` lines are `NAME|pntools|VALUE` keyed by the consts in `lib/env/standard.rs`. `DB/config/<serverid>/meta.pandora` is line-indexed (lines 4-7 and 10 are dead Drive slots that must stay blank-but-present); `helpers::compose_server_meta` is the single writer, and per-site fansub lines are mapped by `pnworker::server_config::FansubSite`. See docs/PROJECT.md for the full index before touching either.

## Conventions

- Comments go *above* functions and explain intent/behaviour; the codebase uses no `///` doc comments.
- Error handling is deliberately loose — `.unwrap()` on regex/IO/JSON expected to succeed, `Result<(), Box<dyn std::error::Error>>` as the common shape, `Send + Sync` added only when a value crosses a spawn boundary.
- `Regex::new(r"...").unwrap()` per call site; `lazy_static`/`once_cell` are not dependencies.
- Async is tokio everywhere: `tokio::fs`, `AsyncWriteExt`, streaming via `resp.chunk().await?`. `src/lib/image/` is sync — wrap it in `spawn_blocking`.
- Logging is `log!(handle_opt, "msg\n")` with `Option<LoggingHandle>`.
- Launch `ffmpeg`/`ffprobe` only through `lib::bin::resolve_runtime_binary` so `DB/bin` portable builds win over PATH.

Multiple agents and the user work in this repo concurrently — expect uncommitted changes from others and re-read files before editing.
