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

`hardware` is what the Pandora Mini scheduler routes on: a `gpu` preset is only ever offered to a
node whose token is marked `gpu`. See [../docs/LINK.md](../docs/LINK.md).

A unit test renders every file here against the built-in it mirrors and fails if the two disagree,
so these stay honest as the built-ins change.
