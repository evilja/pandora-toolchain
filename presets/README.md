# Reference presets

The encoder presets `pndc` ships with, written out in the format `DB/config/global/presets/`
accepts. These files are **reference copies, not live configuration** — Pandora never reads this
directory. Copy one across to activate it:

```sh
mkdir -p DB/config/global/presets
cp presets/standard.toml DB/config/global/presets/
```

A preset with no file under `DB/config/global/presets/` uses the built-in table compiled into the
binary, so an untouched deployment encodes exactly as it always has. A file replaces the built-in
of the same name entirely — there is no merging, which is what keeps an edited preset readable as
the whole truth about that encode.

Three behaviours can be declared. Two have defaults that follow from a preset's own settings; the
third has no default to derive and is off until asked for:

```toml
aot = true        # encode while the source is still downloading
chunked = true    # split the episode across parallel encoders
idle = true       # run only while no ordinary encode does, and pause when one is ordered
```

- **`aot` — encoding ahead of the download.** One ffmpeg process reads the still-growing source and
  runs this preset's own filter and encoder arguments, producing the whole video track; the
  foreground run muxes audio into it. Nothing about that is specific to a codec, so **libx264,
  NVENC, AMF, QSV and VAAPI presets can all do it.** It defaults to on for the CPU presets that do
  not scale. A hardware preset defaults to off — the GPU is shared by the whole node and its
  foreground encode is minutes rather than hours — and a scaling preset defaults to off because
  those are usually run beside a source-resolution encode, where speculating on both doubles the
  load to save the cheaper one. Turn it on with `aot = true` when neither applies.
- **`chunked` — chunking across parallel encoders.** Defaults to on when the x264 `preset` is
  `veryslow` or `placebo`; faster presets keep one continuous encoder so its rate-control state
  survives to the handoff. Declare it when the default reads your preset wrong — it only recognises
  the x264 preset names it was written knowing about, and a preset built on `slower` with a heavy
  `-x264-params` can be slower than a bare `veryslow` and still be read as fast.

- **`idle` — background encoding.** The job is dispatched to a second encode lane (`enc-idle`)
  instead of `enc-main`, and its encoder is fed through the same gate the speculative planners use:
  while any ordinary encode holds `DB/work/.foreground-encode`, it stops handing ffmpeg bytes and
  waits. The encoder process is never killed, so a pause costs nothing and a resume re-encodes
  nothing — rate control, lookahead and frame history are all still in memory. It takes `.aot-owner`
  while running and releases it while paused, so one idle consumer uses the machine at a time and a
  paused one does not deny the budget to download-time speculation. **The pause lives only as long
  as pndc does**: a restart fails the job like any other non-terminal one, and it starts over.
  Nothing derives this — a preset that would take days and one someone is watching are the same
  arguments to ffmpeg — so it is off unless a file says otherwise. An idle preset never chunks,
  whatever it declares: occupying every core is the opposite of what it asked for.

Unlike `aot`, `chunked` cannot be declared onto an encode that is not eligible for it: the chunk
scheduler drives libx264 directly and applies a filter chain of its own, so a preset that is not
`libx264` or that scales encodes linearly however it is declared, and says so on stderr rather than
leaving you waiting for a speedup that was never coming.

The ahead-of-time encoder takes its filter and its encoder arguments from the same file, verbatim,
and records them alongside its output; the foreground encode compares its own against that record
and declines to adopt anything encoded differently. So editing a preset changes both halves of an
encode together, and a preset edited *between* the two halves costs a re-encode rather than
producing one file with two sets of settings in it.

`hardware` is the first Pandora Mini routing filter: a `gpu` preset is only offered to a node whose
token is marked `gpu` or `both`. The node must also prove the exact video encoder with a real test
encode at registration; merely listing it in `ffmpeg -encoders` is not enough. See
[../docs/LINK.md](../docs/LINK.md).

`av1.toml` mirrors the built-in `av1` job preset: AV1 NVENC, `aot = true`, and quality-oriented Ada
settings. Its CQ is hardware/content calibration, not the H.264 QP scale. AV1 release servers must
use either `drive_only:true` for MP4 delivery or `hls:true` for fMP4/CMAF delivery; external
streaming hosts remain disabled because they may transcode the release.

`gpu-nvenc.toml` and `cpu-x265.toml` are the two files here that mirror no built-in, and each is
copied across under the name of the built-in you want it to replace:

```sh
cp presets/gpu-nvenc.toml DB/config/global/presets/gpu.toml       # NVIDIA H.264, in place of AMF
cp presets/cpu-x265.toml  DB/config/global/presets/veryslow.toml  # 10-bit HEVC on the CPU
```

`cpu-x265.toml` is the only preset here that encodes to a bitrate rather than to a quality level:
10-bit H.265 through libx265 at a 9000 kbit/s average with a 12000k ceiling, capped at 1080p,
carrying the source's global metadata and chapter list into the release. It is also the only one
that declares `idle = true`, on the reasoning that an encode at these settings is an archive pass
rather than something anyone is waiting on. It is the one place `extra_args` is load-bearing rather
than decorative — `-b:v`, `-map_metadata` and `-map_chapters` have no fields of their own, because
every built-in is constant-quality and none of them has ever kept chapters. `veryslow` is suggested
as its name because that preset already means "however long it takes"; nothing stops it replacing
another. HEVC releases publish through HLS as MPEG-TS segments, the same path H.264 takes; only AV1
switches to fMP4.

A unit test renders every file here against the built-in it mirrors and fails if the two disagree,
so these stay honest as the built-ins change; every file is also parsed and checked to name an
encoder, including the ones that mirror nothing.
