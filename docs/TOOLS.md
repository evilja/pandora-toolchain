# TOOLS.md

CLI tool flags and ASS parsing details.

## `pncurl` flags

- default: simple GET to `--opcode` path. Client built with `.timeout(Duration::from_secs(600))` — `Req::download` in `lib::http::curl/core.rs`.
- `--drive --env env.pandora`: uploads `--link` (local file) to Drive + Doodstream + Lulu + Voe + Abyss in parallel, streaming progress as protocol opcode `0`, results as opcode `1` per host. Upload sends are unlimited/no request deadline (clients only set a 60s connect timeout); upload streams use 256 KiB read buffers and known content lengths for every provider, including Drive. Source progress events are throttled to 500ms to avoid per-chunk channel traffic; the protocol payload emits the total file size once plus `[done, extension_count]` per host, currently with extension count normally `0`, rendered as e.g. `Doodstream 11/1032 MB`. The first protocol progress payload is emitted immediately; later protocol progress payloads are throttled to roughly 5s from the previous emitted progress.
- `--drive --backup`: Drive-only upload with the same unlimited upload behavior.
- `--gscrape`: Google Drive scraper. Parses the file id from the link, GETs the confirm page, extracts the `uuid` from the form, then GETs the final URL with `confirm=t&uuid=...` and streams chunks to `--opcode`. Client timeout 600s.

## `pnmpeg` intro concat mode

`pnmpeg --concat --input <episode.mp4> --intro-dir <group-folder> --output <video.mp4>` discovers the retained intro variants in the group folder. If one has the same H.264/AAC concat properties as the encoded episode (dimensions, pixel format, sample aspect ratio, frame rate, sample rate, and channel count), both files are joined with video/audio stream copy. Otherwise, only the best source intro is transcoded to those properties as `pnmpeg_compat_<signature>.mp4` in the group folder; that retained variant is then stream-copied and automatically reused by later compatible encodes. Existing `/touchintro` variants remain untouched.

`intros.toml` maps group names directly to these folders. `pndc` startup migrates legacy file-array groups into per-group folders before workers start.

## `pnmpeg` Pandora Studio mode

`pnmpeg --studio --input <manifest.json> --output <video.mp4>` renders a file-backed Pandora Studio snapshot through the normal pnprotocol progress/cancel/log path. The JSON manifest supplies ordered ffconcat video inputs, stable audio tracks, source kind, video preset, total FPS/duration, and an optional preview window.

- Encode-kind full renders use video stream copy and AAC audio; preview windows always use the Dummy libx264 preset.
- Backup-kind full renders use the selected Standard/GPU/PseudoLossless/Dummy video settings without subtitle or intro filters.
- Insert tracks are delayed and mixed over base audio. Override tracks additionally mute base audio for their clipped placement intervals. Duck tracks lower every other source to their configured target percentage, with symmetric fade-down/fade-up times clamped to half the duck track duration; overlapping duck envelopes multiply. A source with no audio receives duration-matched stereo silence.
- Every track applies its cumulative start/end cuts and own 0-200% volume, is normalized to 48 kHz stereo, mixed with a limiter, and clipped to the video or preview duration.
- Preview input seeking is applied before the concat source and track trims/delays are made relative to the preview window. Invalid manifests and concat-list failures exit nonzero so the worker reports failure rather than uploading a missing output.

## `ffmpeg` preview screenshots

`/smartcode exp` uses `lib::mpeg::preview::ffmpeg_screenshot` through the probe worker after the normal download/cache path finishes. For each selected TS line midpoint it runs one bounded ffmpeg frame extraction with subtitles burned in:

`ffmpeg -y -ss <seconds> -copyts -i <input> -vf subtitles=f=<subtitle.ass>:fontsdir=<work/fonts> -frames:v 1 -update 1 <out.png>`

The worker stages fonts referenced by the merged ASS from `DB/fontconfig/<server_id>` and `DB/fontconfig/global` into one `work/fonts` directory for libass. The overlay/watermark is drawn afterward with `src/lib/image/`. `/cfont` stores the requested watermark font in `DB/config/<server_id>/preview.toml`; the default requested face is `Gandhi Sans Bold`, which must be installed with `/font` if the operator wants that exact font. If no configured/default face resolves, rendering uses the embedded Liberation Mono fallback.

## `pnass` flags

Always emits a pnprotocol negotiation line on stdout (`PNprotocol:PNdc@0.1.1@1:PNass@0.1.1@1:PNass` by default; `--negkey` / `--negotiator` / `--negver` override the three pieces). Emits line-length warnings as protocol opcode `4` (one per warning event, with grouping for consecutive events — see [pnass line-length check](#pnass-line-length-check)).

- `--input <path>` / `--output <path>` — required. Reads via `SubstationAlpha::load(path, true)` (adv_parsing — events get parsed Override blocks), writes via `dump_to_file`.
- `--merge <path>` — optional secondary ASS to merge into `--input`. When set, the intersection of TL/TS style names drives a per-style rename of the secondary (TL styles stay intact), then TS's styles and events are appended after TL's. See [pnass `--merge` semantics](#pnass---merge-semantics).
- `--inject <path> --duration-centiseconds <N>` — injects a server watermark after the main subtitle using the same resolution and style-collision checks as `--merge`. Watermark events append after main events; `[all]` Effect spans `0:00:00.00` through the supplied duration, while `[precise]` and other/empty Effects retain their own timings.
- `--set-layer <N>` — when set, walks every `Event` and assigns `layer = N`.
- `--smart-layer <N>` — sign-aware layer normalization for smartcode: only events whose style name does not contain `Sign` and whose parsed text contains only raw text plus basic bold/italic/underline/strikeout overrides get `layer = N`; events with positioning, drawings, clips, colours, transforms, reset tags, etc. keep their original layer.
- `--split-signs <path>` — split sign-style events (style name contains `Sign`) from `--input` into a separate ASS at `<path>`, leaving non-sign events in `--output`; used by smartcode when the repo has TL but no TS.
- `--wrap-style <dont_touch|0|1|2|3>` — controls whether `ScriptInfo.wrap_style` is forced during pnass output. Missing/`dont_touch` preserves the loaded value; numeric values force that WrapStyle. `/configure` and `/edit` store this per server.
- `--title <S>` — optional. When provided, overwrites `ScriptInfo.title`. When absent, the loaded title is preserved.
- The other `ScriptInfo` fields (`ScriptType`, `ScaledBorderAndShadow`, `PlayResX/Y`, `YCbCr Matrix`, `LayoutResX/Y`) only get default-filled if they were missing/zero in the loaded file. `LayoutResX/Y` defaults to `PlayResX/Y` (not 1920/1080). `WrapStyle` is not forced unless `--wrap-style` is numeric.
- `--negkey` / `--negotiator` / `--negver` — protocol negotiation overrides. Default `negotiator`/`negver` are `"PNass"` / `"0.1.1"`; default `negkey` is `"PNassCLI"`. The worker's injection spec uses `PNassEffects`.

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

Font name reads use `libkagami::core::cached_normalized_font_names`: an in-memory, process-lifetime cache keyed by path metadata `(mtime, len)`. It stores normalized font names and is shared by release font lookup and `/fontcheck`; directory enumeration itself is intentionally uncached.

- `\{` is always a literal `{` — never starts an override block.
- `\}` is literal `{`/`}` outside a block; inside a block, closes it.
- A bare `{` opens an override block; the matching `}` (depth-back-to-zero) closes it. Contents are parsed as `ASSOverride` tags + literal text segments.
- A bare `{` appearing inside an existing block invalidates the entire block: the outer `{` and its matching `}` are dropped, yielding an empty event.
- A lone `{` (no matching `}` to end of string) is a literal `{` (look-ahead via the `find_block_end` helper).
- Raw text outside blocks and inside blocks (around tags) is `ASSLine::RawText(String)`. Override tags are `ASSLine::Override(ASSOverride::*)`.
