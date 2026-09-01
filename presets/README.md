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

Two behaviours have defaults that follow from a preset's own settings, and both can be declared:

```toml
aot = true        # encode while the source is still downloading
chunked = true    # split the episode across parallel encoders
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

`gpu-nvenc.toml` and `gpu-qsv-hevc.toml` are the two files here that mirror no built-in. There is
no `nvenc` or `qsv` preset name a job can ask for, so each is copied across *as* `gpu.toml` to make
the `gpu` preset encode with that machine's own encoder:

```sh
cp presets/gpu-nvenc.toml    DB/config/global/presets/gpu.toml   # NVIDIA, H.264
cp presets/gpu-qsv-hevc.toml DB/config/global/presets/gpu.toml   # Intel iGPU, H.265
```

`gpu-qsv-hevc.toml` is the only preset here that encodes to a bitrate rather than to a quality
level: HEVC through Quick Sync at a 9000 kbit/s average with a 12000k ceiling, capped at 1080p. It
is the one place `extra_args` is load-bearing rather than decorative — `-b:v` has no field of its
own, because every built-in is constant-quality. HEVC releases publish through HLS as MPEG-TS
segments, the same path H.264 takes; only AV1 switches to fMP4.

A unit test renders every file here against the built-in it mirrors and fails if the two disagree,
so these stay honest as the built-ins change; every file is also parsed and checked to name an
encoder, including the ones that mirror nothing.
