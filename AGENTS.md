# AGENTS.md

Guidance for coding agents working in this repository.

- **docs/PROJECT.md** — project layout, build/verify steps, general conventions, and environment/runtime layout.
- **docs/DISCORD.md** — Discord commands, authorization tiers, presence updates, and the `/job` and `/smartcode` flows.
- **docs/WORKER.md** — worker runtime patterns, tool orchestration, torrent routing, and cache/duplicate behavior.
- **docs/API.md** — HTTP API routes, auth/tokens, web console behavior, and deployment notes.
- **docs/TOOLS.md** — `pncurl`, `pnass`, libkagami parsing, and ASS line-length checks.
- **docs/LOCALIZATION.md** — message IDs, TOML translation files, and how to add new strings.

## Build / verify

- Build: `cargo build` (full workspace) or `cargo build --bin <name>`.
- Lint check: `cargo check --all-targets`.
- Tests: `cargo test --lib` (mostly in `lib::p2p::nyaaise::tests`).
- No formatter / clippy config is enforced; match surrounding style.

After any change, run `cargo check --all-targets` at minimum.

## Attention

More than one AI agent and the user works on this project. Changes might be coming from them.
