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

Two behaviours follow from a preset's own settings:

- **Encoding ahead of the download** happens when the codec is `libx264` and the filter chain has
  no `scale=`. This one is not declarable: a preset that scales must run its filter exactly once,
  in the encode that writes the output, or the picture is resampled twice.
- **Chunking across parallel encoders** defaults to on when the x264 `preset` is `veryslow` or
  `placebo`. Faster presets keep one continuous encoder so its rate-control state survives to the
  handoff.

Chunking can be declared outright, because that default only recognises the x264 preset names it
was written knowing about — a preset built on `slower` with a heavy `-x264-params` can be slower
than a bare `veryslow` and still be read as fast:

```toml
chunked = true    # or false to keep one encoder at an otherwise veryslow-class preset
```

It is a choice between the two schedules an encode is already eligible for, not a way past
eligibility: a preset that is not `libx264`, or that scales, cannot chunk however it is declared.
One that asks anyway says so on stderr and encodes linearly.

The ahead-of-time encoder reads its CRF, x264 preset and params back out of the same file, so
editing one of these changes both halves of an encode together.

`hardware` is what the Pandora Mini scheduler routes on: a `gpu` preset is only ever offered to a
node whose token is marked `gpu`. See [../docs/LINK.md](../docs/LINK.md).

A unit test renders every file here against the built-in it mirrors and fails if the two disagree,
so these stay honest as the built-ins change.
