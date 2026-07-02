# TOOLS.md

CLI tool flags and ASS parsing details.

## `pncurl` flags

- default: simple GET to `--opcode` path. Client built with `.timeout(Duration::from_secs(600))` — `Req::download` in `libpncurl/core.rs`.
- `--drive --env env.pandora`: uploads `--link` (local file) to Drive + Doodstream + Lulu + Voe + Abyss in parallel, streaming progress as protocol opcode `0`, results as opcode `1` per host. Upload sends are unlimited/no request deadline (clients only set a 60s connect timeout); progress payload emits the total file size once plus `[done, extension_count]` per host, currently with extension count normally `0`, rendered as e.g. `Doodstream 11/1032 MB`. The first progress payload is emitted immediately; later progress payloads are throttled to roughly 5s from the previous emitted progress.
- `--drive --backup`: Drive-only upload with the same unlimited upload behavior.
- `--gscrape`: Google Drive scraper. Parses the file id from the link, GETs the confirm page, extracts the `uuid` from the form, then GETs the final URL with `confirm=t&uuid=...` and streams chunks to `--opcode`. Client timeout 600s.

## `pnass` flags

Always emits a pnprotocol negotiation line on stdout (`PNprotocol:PNdc@0.1.1@1:PNass@0.1.1@1:PNass` by default; `--negkey` / `--negotiator` / `--negver` override the three pieces). Emits line-length warnings as protocol opcode `4` (one per warning event, with grouping for consecutive events — see [pnass line-length check](#pnass-line-length-check)).

- `--input <path>` / `--output <path>` — required. Reads via `SubstationAlpha::load(path, true)` (adv_parsing — events get parsed Override blocks), writes via `dump_to_file`.
- `--merge <path>` — optional secondary ASS to merge into `--input`. When set, the intersection of TL/TS style names drives a per-style rename of the secondary (TL styles stay intact), then TS's styles and events are appended after TL's. See [pnass `--merge` semantics](#pnass---merge-semantics).
- `--set-layer <N>` — when set, walks every `Event` and assigns `layer = N`.
- `--smart-layer <N>` — sign-aware layer normalization for smartcode: only events whose style name does not contain `Sign` and whose parsed text contains only raw text plus basic bold/italic/underline/strikeout overrides get `layer = N`; events with positioning, drawings, clips, colours, transforms, reset tags, etc. keep their original layer.
- `--split-signs <path>` — split sign-style events (style name contains `Sign`) from `--input` into a separate ASS at `<path>`, leaving non-sign events in `--output`; used by smartcode when the repo has TL but no TS.
- `--wrap-style <dont_touch|0|1|2|3>` — controls whether `ScriptInfo.wrap_style` is forced during pnass output. Missing/`dont_touch` preserves the loaded value; numeric values force that WrapStyle. `/configure` and `/edit` store this per server.
- `--title <S>` — optional. When provided, overwrites `ScriptInfo.title`. When absent, the loaded title is preserved.
- The other `ScriptInfo` fields (`ScriptType`, `ScaledBorderAndShadow`, `PlayResX/Y`, `YCbCr Matrix`, `LayoutResX/Y`) only get default-filled if they were missing/zero in the loaded file. `LayoutResX/Y` defaults to `PlayResX/Y` (not 1920/1080). `WrapStyle` is not forced unless `--wrap-style` is numeric.
- `--negkey` / `--negotiator` / `--negver` — protocol negotiation overrides. Default `negotiator`/`negver` are `"PNass"` / `"0.1.1"`; default `negkey` is `"PNassCLI"`.

Exit non-zero on `dump_to_file` failure.

## `pnass` line-length check

After loading with `adv_parsing=true`, pnass walks every `Event` and emits warnings for visible text lines longer than 50 characters. The check uses libkagami's parsed structure directly, not a regex on raw bytes.

- For each event, the `text.data: Vec<ASSText>` is walked. Override block contents are skipped (only `ASSText::RawText` segments contribute to visible length). Inside a `RawText`, the text is split on `\N` (hard line break) and each segment is measured.
- Drawing-mode events are skipped: any event whose `text.data` contains `ASSText::Override(ASSOverride::P(1))` is ignored.
- For each long segment, a warning is emitted via `pn_emit!` with opcode `4` and two leaves: `"{event_number}: {visible line}"` for the first warning of a run; the rest of the run collapses into `"N more similar warnings"` emitted once at the end of the run (or per-event if the run never repeats).
- A "run" is a contiguous block of events that each emit at least one warning; a non-warning event or EOF flushes the current run.

Consumed by pnass-driven flows such as `/merge` / `/smartcode`; `/job` no longer runs pnass, so it does not surface line-length warnings.

## libkagami override-block parsing

ASSLine parser (`from_str_store` / `FromStr::from_str` in `src/libkagami/tags/mod.rs`) follows Aegisub's override-block rules. Used by `pnass` when `adv_parsing=true` is passed to `SubstationAlpha::load`.

- `\{` is always a literal `{` — never starts an override block.
- `\}` is literal `{`/`}` outside a block; inside a block, closes it.
- A bare `{` opens an override block; the matching `}` (depth-back-to-zero) closes it. Contents are parsed as `ASSOverride` tags + literal text segments.
- A bare `{` appearing inside an existing block invalidates the entire block: the outer `{` and its matching `}` are dropped, yielding an empty event.
- A lone `{` (no matching `}` to end of string) is a literal `{` (look-ahead via the `find_block_end` helper).
- Raw text outside blocks and inside blocks (around tags) is `ASSLine::RawText(String)`. Override tags are `ASSLine::Override(ASSOverride::*)`.
