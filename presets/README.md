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

Two behaviours follow from a preset's own settings rather than from anything you declare:

- **Encoding ahead of the download** happens when the codec is `libx264` and the filter chain has
  no `scale=`. A preset that scales must run its filter exactly once, in the encode that writes the
  output, or the picture is resampled twice.
- **Chunking across parallel encoders** happens when the x264 `preset` is `veryslow` or `placebo`.
  Faster presets keep one continuous encoder so its rate-control state survives to the handoff.

The ahead-of-time encoder reads its CRF, x264 preset and params back out of the same file, so
editing one of these changes both halves of an encode together.

`hardware` is what the Pandora Mini scheduler routes on: a `gpu` preset is only ever offered to a
node whose token is marked `gpu`. See [../docs/LINK.md](../docs/LINK.md).

A unit test renders every file here against the built-in it mirrors and fails if the two disagree,
so these stay honest as the built-ins change.
