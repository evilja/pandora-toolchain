# AGENTS.md

Guidance for coding agents working in this repository.

- **docs/PROJECT.md** — project layout, build/verify steps, general conventions, and environment/runtime layout.
- **docs/DISCORD.md** — Discord commands, authorization tiers, presence updates, and the `/job` and `/smartcode` flows.
- **docs/WORKER.md** — worker runtime patterns, tool orchestration, torrent routing, and cache/duplicate behavior.
- **docs/API.md** — HTTP API routes, auth/tokens, web console behavior, and deployment notes.
- **docs/TOOLS.md** — `pncurl`, `pnass`, libkagami parsing, and ASS line-length checks.
- **docs/LUMIERE_BROKER.md** — Cloudflare Worker deployment, Drive profiles, secret migration, and VDS upload data flow.
- **docs/LOCALIZATION.md** — message IDs, TOML translation files, and how to add new strings.

## `lumiere-internal/` — never commit this

This repository is **public**. `lumiere-internal/` is the gitignored home for everything that must
stay private: security audits and their findings, roadmaps, plans, review reports, notes on unfixed
weaknesses, and anything else that would help someone attack the deployment. Write those here, not
at the repo root and not in `docs/`.

- **Do not commit anything from it, and do not un-ignore it.** A force-push does not undo a leak —
  GitHub keeps unreachable commits reachable by SHA long after the branch stops pointing at them.
- **Do not copy its contents into tracked files**, commit messages, or PR descriptions. Referring to
  a finding by its id (`SOL-S1`) is fine; restating the exploit is not.
- `docs/` describes how the project *behaves* and is public. `lumiere-internal/` describes what is
  *wrong with it* and is not. When a finding is fixed, the fix and its rationale belong in `docs/`
  and the commit message; the report stays here.
- If asked to produce a report, roadmap, audit, or plan, default to writing it in
  `lumiere-internal/` without being told.

## Build / verify

- Build: `cargo build` (full workspace) or `cargo build --bin <name>`.
- Lint check: `cargo check --all-targets`.
- Tests: `cargo test --lib` (mostly in `lib::p2p::nyaaise::tests`).
- No formatter / clippy config is enforced; match surrounding style.

After any change, run `cargo check --all-targets` at minimum.

## Attention

More than one AI agent and the user works on this project. Changes might be coming from them.
