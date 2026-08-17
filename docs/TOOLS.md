# TOOLS.md

CLI tool flags and ASS parsing details.

## `pntrace`

The image tracer has a Pandora-side development server that uses libkagami but does not start or connect to PNdc:

```text
cargo run --bin pntrace
```

The same lab is available from Pandora at `/trace`; it shares the browser's `pandora_token` and calls the bearer-protected `/api/v1/trace` and `/api/v1/trace/ass` endpoints. The desktop includes it as a Trace app. Standalone `pntrace` remains intentionally loopback-oriented and unauthenticated for development.

It binds `127.0.0.1:8788` by default (`--host` / `--port` override it), serves the drag-and-drop trace lab at `/`, accepts raw encoded image bodies at `POST /api/trace`, and converts a trace model at `POST /api/ass`. The ASS route accepts `{ trace, filename?, duration_centiseconds?, seam_overlap? }` (five seconds and a 0.5px overlap by default) and always returns `application/zip` with exactly one sanitized `.ass` entry; there is no raw-ASS page endpoint. Query fields accept `preset` (`logo_ui`, `illustration`, `photo`, or `gradient`) plus `TraceOptions` overrides: `color_count` (1–512), `preserve_gradients`, `color_smoothing`, `path_simplify`, `curve_fit`, `corner_threshold`, `min_area`, `alpha_threshold`, `max_dimension`, and SVG-only `svg_seam_overlap`. The trace response contains `{ trace, svg, elapsed_ms }`; the page previews both source and SVG, toggles palette layers, downloads SVG or versioned trace JSON, and offers `Get as ASS (.zip)` through the real libkagami adapter. The Gradient preset uses higher palette density, exact low-complexity histogram colors, source-space palette reconstruction, boundary-preserving curve fitting, and a restrained 0.25px SVG underlap for subtle ramps. SVG overlap is independently adjustable so cracks can be closed without unnecessarily swelling small artwork. The page's ASS seam-overlap control sends `0`–`4` pixels to the ZIP endpoint; `0.5` is the default. See `kagami-trace/README.md` for the portable model and limits.

Libkagami's `libkagami::tracing::{parse_trace_json, trace_to_ass, trace_json_to_ass}` adapter turns this model into one ASS drawing event per color layer. It maps RGBA to ASS BGR + inverted alpha, emits fitted cubic curves as ASS `b` commands, retains contour winding for holes, and uses `TraceAssOptions` for timing/layer/style fields. Libkagami compacts consecutive line and cubic coordinates under ASS's persistent `l` / `b` modes instead of repeating a mode before every segment. `seam_overlap` defaults to `0.5`, drawing a same-color ASS outline under each fill so independently antialiased regions cannot expose background gaps at shared edges; set it to `0.0` to disable the underlap:

```rust,no_run
use pandora_toolchain::{
    kagami_trace::{TraceOptions, TracePreset, trace_image},
    libkagami::{
        complex::types::AssTime,
        tracing::{TraceAssOptions, trace_to_ass},
    },
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let image = std::fs::read("logo.png")?;
    let trace = trace_image(
        &image,
        &TraceOptions::for_preset(TracePreset::LogoUi),
    )?;
    let ass = trace_to_ass(&trace, &TraceAssOptions {
        end: AssTime::from_centiseconds(1_000),
        ..TraceAssOptions::default()
    })
    .map_err(std::io::Error::other)?;
    std::fs::write("logo.ass", ass.stringify())?;
    Ok(())
}
```

For JSON downloaded from the trace lab, call `trace_json_to_ass(&json, &options)` instead. Defaults produce source-sized PlayRes, one event per color, and a five-second `0:00:00.00`–`0:00:05.00` duration.

## `pnp2p` selection flags

- `--select <index>` downloads exactly one file of the torrent; unchanged, and still what `/encode pan` and `/backup` use.
- `--selects <a,b,c>` downloads a whole selection in **one** process — the info-hash lock allows only one downloader per torrent, so a batch cannot be several `pnp2p` calls. Whitespace is tolerated, duplicates and unparsable entries are dropped, and `--select` may be combined with it. In this mode `pnp2p` emits opcode `6` (`["6", [index, name]]`) the moment a file's last piece is written and flushed, before the rest of the selection finishes; opcodes `0`/`1`/`2`/`3`/`5` keep their usual meanings. `--probe` and the whole-torrent mode are unaffected.

## `pncurl` flags

- default: simple GET to `--opcode` path. Client built with `.timeout(Duration::from_secs(600))` — `Req::download` in `lib::http::curl/core.rs`.
- `--drive --env env.pandora`: legacy standalone compatibility mode that reads reusable credentials from the supplied file. It uploads `--link` to Drive + Lulu + Voe in parallel, emitting initialization as protocol opcode `4`, progress as opcode `0`, and results as opcode `1`/`2` per host. Doodstream and Abyss were removed from this mode with the providers themselves; the surviving host ids (`1` Drive, `4` Lulu, `5` Voe) keep their existing values rather than being renumbered, so `2` and `6` are simply gone and the emitted progress tuple now carries three host pairs instead of five. Upload sends have no request deadline, use 256 KiB read buffers and known content lengths, throttle source events to 500ms and protocol progress to roughly 5s, and cache resolved Drive paths under `DB/cache/drive-folders`. `pndc` does not invoke this mode; worker uploads use integrated Lumiere instead. Do not configure this mode after removing VDS provider credentials.
- `--drive --backup`: legacy Drive-only compatibility mode with the same upload behavior and credential restriction.
- `--gscrape`: Google Drive scraper. Parses the file id from the link, GETs the confirm page, extracts the `uuid` from the form, then GETs the final URL with `confirm=t&uuid=...` and streams chunks to `--opcode`. Client timeout 600s.

## `pnmpeg --extractsubs`

`pnmpeg --extractsubs --input <video> --output <dir>` writes every text subtitle track of a container to `<dir>` and reports one opcode `4` row per track as `[ordinal, language, title, codec, filename, detail]`. `ordinal` is the position among subtitle streams — what `-map 0:s:N` selects — not the global stream index, which counts video and audio too. A row with an empty `filename` was skipped and `detail` says why; a row with a filename was extracted and `detail` is empty. Opcode `1` ends the run even when nothing was extractable, so a container of image-only tracks explains itself instead of failing.

Codec mapping lives in `lib::mpeg::subs`: `ass`/`ssa` → `.ass`, `subrip`/`text` → `.srt`, `webvtt` → `.vtt`, `microdvd` → `.sub`, and `mov_text` → `.srt`. Everything is stream-copied except `mov_text`, which is MP4's own text format and has to be transcoded on the way out. Image-based codecs (`hdmv_pgs_subtitle`, `dvd_subtitle`, …) map to nothing and are skipped. ffmpeg exits 0 on an empty track, so a zero-byte result is deleted and reported as skipped rather than handed on as a usable subtitle.

Filenames are `<ordinal>.<language>.<title-slug>[.forced].<ext>`. The ordinal leads because it is the only guaranteed-unique part, and language and title are reduced to an alphanumeric slug — they are metadata inside someone else's file, so they are never allowed to reach the filesystem unfiltered.

## `pnmpeg` intro concat mode

`pnmpeg --concat --input <episode.mp4> --intro-dir <group-folder> --output <video.mp4>` discovers the retained intro variants in the group folder. If one has the same H.264/AAC concat properties as the encoded episode (dimensions, pixel format, sample aspect ratio, frame rate, sample rate, and channel count), both files are joined with video/audio stream copy. Otherwise, only the best source intro is transcoded to those properties as `pnmpeg_compat_<signature>.mp4` in the group folder; that retained variant is then stream-copied and automatically reused by later compatible encodes. Existing `/touchintro` variants remain untouched.

`intros.toml` maps group names directly to these folders. `pndc` startup migrates legacy file-array groups into per-group folders before workers start.

## `pnmpeg` Pandora Studio mode

`pnmpeg --studio --input <manifest.json> --output <video.mp4>` renders a file-backed Pandora Studio snapshot through the normal pnprotocol progress/cancel/log path. The JSON manifest supplies ordered ffconcat video inputs, stable audio tracks, source kind, video preset, total FPS/duration, and an optional preview window.

- Encode-kind full renders use video stream copy and AAC audio; preview windows always use the Dummy libx264 preset.
- Backup-kind full renders use the selected Standard/VerySlow/GPU/PseudoLossless/Dummy video settings without subtitle or intro filters.
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
- `--logfile <path>` — optional write-through run log (`lib::logging::tool::ToolLog`): one timestamped line per step (input/secondary load with their event counts, injection timings, merge result, warning scan, write, done) plus a line for any error path, flushed as it happens rather than buffered to the end, because the run worth reading is the one that never finishes. Parent directories are created. `PNASS_INJECT` passes `DB/work/<job_id>/log/PNass_Inject<job_id>.log`; the other pnass specs do not set it yet.
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

## Tool run logs

Every tool now writes a **run log**: `ToolLog` in `src/lib/logging/tool.rs`, one line per step with the elapsed time since the tool started, each written *and flushed* as it happens. `LoggingHandle` buffers 5000 bytes and only ever held the subprocess transcript, so a tool that hung left nothing at all on disk — which is exactly what made a stalled encode unreadable. A run log that simply stops names the step that never returned.

`ToolLog::beside(logfile)` derives `<name>.run.log` next to the tool's existing `--logfile` transcript, so pnmpeg and pncurl gained one without a new `CliParam`. pnass and pnp2p take `--logfile` directly (pnass has no subprocess transcript; pnp2p had no log at all).

- **pnmpeg** → `PNmpeg_*<job_id>.run.log`. Args and mode, intro preparation (which may transcode), `select_subinput`, the framerate/samplerate compatibility probe, preset parameter count, the audio-language probe, **every `ffprobe -count_packets` with its duration** — a full demux of the input, the longest thing that runs before ffmpeg exists — then `handing off to ffmpeg`, `spawning ffmpeg`, the **first progress frame**, and the terminal warning/done/fail/cancel. Everything before "first ffmpeg progress" is setup a stalled run never got past.
- **pnp2p** → `PNp2p*<job_id>.log`. Args, client initialisation, probe start/result count, the selection being downloaded, and the terminal result. Previously the torrent path wrote nothing to the job directory, so a stuck download and a download that never started were indistinguishable.
- **pncurl** → `PNcurl*<job_id>.run.log`. Args, which mode started (scrape / direct / drive upload), and its outcome.
- **pnass** → `PNass_Inject<job_id>.log`. See [`pnass` flags](#pnass-flags).
