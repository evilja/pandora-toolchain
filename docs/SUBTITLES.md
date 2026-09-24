# Pandora Subs — the browser subtitle editor

`GET /subs` serves a subtitle editor in the spirit of Aegisub, built for phones first and laid out
like Aegisub on a wide screen. It is a **client-only** page: scripts are opened from the device,
edited in the browser, autosaved to the browser's IndexedDB, and downloaded or shared back. Nothing a
user opens is uploaded, and editing needs no token. The one server feature is fetching a video from
a torrent, magnet, nyaa, Drive or direct link — see [Video from a link](#video-from-a-link) — which
uses the console's signed-in token.

## Files

| File | Route | What it is |
|---|---|---|
| `web/subs.html` | `GET /subs` | Markup and all CSS. Takes every colour from the shell tokens (`/console.css`) but keeps its own compact chrome instead of `PN.shell()` — the rail and topbar would take a third of a phone screen. |
| `web/subs/ass.js` | `GET /subs/ass.js` | The pure core (`window.ASS`, or `module.exports` under Node): parse/serialize, tag surgery, HYDRA, timing and line tools, resampling, find/select. No DOM. |
| `web/subs/render.js` | `GET /subs/render.js` | `window.SubRender.render(canvas, doc, timeMs, opts)` — a canvas approximation of libass for the preview. |
| `web/subs/app.js` | `GET /subs/app.js` | State, undo history, autosave, every view and tool sheet, and the link-to-video flow. |
| `web/subs/manifest.webmanifest` | `GET /subs/manifest.webmanifest` | Lets the page be added to a phone's home screen as a standalone app. |
| `web/subs/ass.test.js` | — | `node web/subs/ass.test.js` runs the core's tests; no dependencies. |

All five routes are unauthenticated page routes, `include_str!`-baked into `pndc` like the other
consoles, and served `no-cache`. The page asks for its scripts, manifest and `/console.{css,js}` as
`?v=<hash of the embedded files>`, so a new build is a new URL: Safari otherwise kept running the
previous deploy's `app.js` across ordinary reloads.

## Model

- A document is `{ info, styles, events, extras }`. `info` is an ordered key/value list, `extras`
  keeps any section the editor does not understand (`[Fonts]`, `[Graphics]`, `[Aegisub Project
  Garbage]`, …) and writes it back in its original place.
- Events and styles are **immutable** — every edit replaces the object — so an undo step is a
  shallow copy of the arrays, and derived values (CPS, line length, list preview) are cached per
  event object in a `WeakMap`.
- Times are integer milliseconds and are rounded to centiseconds only when written. Frame snapping
  uses the configured frame rate (23.976 by default, or detected from the video with
  `requestVideoFrameCallback`).
- Colours are `&HAABBGGRR` in styles and `&HBBGGRR&` in tags; decimal SSA colours are accepted on
  read.
- Input: `.ass`, `.ssa` (v4 alignment converted), `.srt`, `.vtt`, as UTF-8, UTF-16 with a BOM, or
  Windows-1254 (older Turkish releases) when strict UTF-8 fails. Output: `.ass` (UTF-8 BOM, CRLF) or
  `.srt`.

## Layout

- **Phone (< 1100px):** one pane at a time behind a bottom tab bar — Lines, Edit, Video, Styles,
  Tools. The Edit pane keeps a collapsible video strip on top. Tools and dialogs open as bottom
  sheets. Inputs are 16px (no iOS zoom-on-focus), touch targets are at least 44px, and the tab bar
  hides while the on-screen keyboard is up.
- **Desktop (≥ 1100px):** video + waveform top-left, the edit box top-middle, the line grid across
  the bottom, and Styles/Tools in a right-hand column.

## Behaviour worth knowing

- **Line list** is virtualised (fixed row height), so a 5,000-line karaoke script scrolls fine on a
  phone. Tap selects, tapping the selected line opens it, long-press starts multi-select;
  Shift/Ctrl-click work on desktop. Rows flag overlaps (amber times), CPS over the limit, and lines
  over the length limit (default **50** visible characters — the same rule as `pnass`, see
  [TOOLS.md](TOOLS.md)).
- **Enter** in the text box commits and moves to the next line (creating one at the end);
  Shift+Enter types `\N`. Settings can swap the two.
- **Undo** coalesces a burst of the same edit (typing into one line, dragging one handle) into one
  step and keeps 300 steps.
- **Autosave** writes the current script to IndexedDB about 1.5 s after each change and whenever the
  page is hidden, keeps the 20 most recent scripts under *Recent scripts*, and reopens the last one
  on load. The header shows *not downloaded* until the `.ass` is saved out.
- **Video** is a local file, a direct link, or a link the server fetched (below); the preview is drawn on a canvas over it at the
  script's `PlayRes`. With no video it draws on black and a virtual clock drives playback. The
  `\pos` tag-bar button turns the video into a tap/drag target that writes `\pos` for the line.
- **Waveform** decodes the media's audio once (`decodeAudioData` at a low sample rate) into 10 ms
  peaks and drops the PCM. Drag the green/red handles to time the line (snapping to nearby line
  edges, the playhead, and frames), drag elsewhere to scroll, pinch or Ctrl+wheel to zoom. Files
  over 900 MB are refused — a phone cannot hold them decoded — so a large MKV needs an audio-only
  copy.

## Video from a link

*Open video from a link or torrent* takes what `/encode` takes: a nyaa page, a `.torrent` URL, a
magnet, a Google Drive link, or any other direct link. A link ending in `.mp4`, `.webm`, `.m3u8` or
a playable audio extension still opens straight in the `<video>` element; everything else goes to
the server, because a browser cannot play a torrent and a phone cannot decode the 10-bit HEVC MKV
behind most of them.

1. Without a token (`localStorage["pandora_token"]`) a sheet explains why and links to `/login`.
2. A nyaa, `.torrent` or magnet link is probed first (`POST /api/v1/jobs/probe`). With more than
   one video file in it, the user picks one; the list keeps only video extensions when there are
   any.
3. `POST /api/v1/subs/media` with `{ torrent }` or `{ probe_job_id, file_index }` queues a
   `SubsMedia` job, which downloads through the normal pipeline and then makes a 540p H.264/AAC
   proxy (`-fps_mode passthrough`, so frame times are the source's), 10 ms waveform peaks, and the
   file's text subtitle tracks — see [WORKER.md](WORKER.md#subtitle-extraction).
4. The page polls `GET /api/v1/jobs/:id` every 2 s and shows a progress pill under the header
   (queue → download → convert). Tapping it offers *Stop*, which cancels the job. The job being
   followed is kept in `localStorage["pandora_subs_remote"]`, so a phone that drops the tab resumes
   following it on the next visit.
5. When it reaches `Uploaded`, `progress.token` names the result. The video plays from
   `/subs/media/<token>/video.mp4`; `peaks.bin` (one byte per 10 ms, scaled to the loudest) becomes
   the waveform without decoding anything on the device; the frame rate is taken from the
   manifest; and if the file carried subtitle tracks, a sheet offers to open one as a new script
   (also under *Subtitle tracks in this video…* in the menus).

The token is saved with the script in IndexedDB, so reopening the script — or reloading the page —
reattaches the video while the server still has it (12 hours).

## Tools

| Tool | What it does |
|---|---|
| HYDRA | Tick tags (colours, alphas, border/shadow, blur, font/scale/spacing, rotations, shear, `\an`, `\q`, `\b\i\u\s`, `\fn`) and apply them at the line start, at the cursor, as a `\t` transform (optional t1/t2/accel), as a per-character gradient, or as a gradient across the selected lines. Remembers the last setup. |
| Select lines | Selectrix-style: field × condition × value (text, visible text, style, actor, effect, layer, CPS, length, duration, start; contains/equals/regex/numeric comparisons), with replace/add/remove/intersect, plus quick picks (overlaps, too fast, too long, comments, empty, same style/actor, zero duration). |
| Find & replace | Plain or regex, case-sensitive or not, outside tags or in the raw line, over text/actor/style/effect. |
| Sort lines | Stable sort by start, end, duration, style, actor, effect, layer, text, CPS, length, or comments-last; the whole file or just the selected rows' slots. |
| Shift times | By a time or a number of frames, earlier or later, moving start/end/both, for the selection, the selection and everything after, or all. |
| Timing post-processor | Lead-in/lead-out, link gaps under a threshold at an adjustable meeting point, same-style only, snap to frames. |
| Reading speed | Lengthen lines to a target CPS with a minimum duration, never past the next line's start. |
| Line breaker | Balance long lines with `\N`, rebalance all, or remove breaks. |
| Clean up | Merge and drop redundant tags, strip all or named tags, remove `{notes}`, trim spaces, delete empty or duplicate lines. |
| Fade / Change case | `\fad` in/out (or remove); sentence/title/upper/lower case on visible text only. |
| Resample | Rescale styles, margins, `\pos`/`\move`/`\org`/`\clip`, drawings, sizes and borders to a new `PlayRes`. |
| Quality check | Lists overlaps, fast and long lines, zero durations, empty lines, missing styles, unbalanced braces, double spaces and sub-half-second lines; tap one to jump to it. |
| Styles | List with swatches and usage counts; editor sheet with a live rendered preview; new/copy/delete (reassigning its lines); renaming updates every line; import from another `.ass` with optional rescale. |

## Limits

The preview is an approximation of libass, not libass: fonts come from the browser (a font the
device does not have falls back), and effects such as `\blur` use the canvas filter rather than a
true Gaussian on the outline. Final checks belong in a real renderer. Browsers only play codecs they
support — a local MKV/HEVC often needs a remux to MP4 or WebM, or can be opened from its link
instead so the server converts it. The server's proxy is 540p: good for timing and typesetting
positions (the preview scales to `PlayRes`), not for judging fine detail.
