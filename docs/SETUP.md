# SETUP.md

First-run configuration for `pndc`, as a coordinator or as a Pandora Mini node.

## Why it exists

Nothing ever created `env.pandora`. The migration at startup only moves one that already exists, so
a fresh install reached serenity with an empty Discord token and failed with a library error that
named nothing an operator could act on — and under `start.sh`'s restart loop that is an unattended
spin. A node was quieter and no clearer: one line saying `link_coordinator_url is not set`, then
exit.

## What triggers it

`ensure_configured()` runs from `pndc::main` before anything reads configuration.

- **Automatically**, when a setting the process cannot start without is missing. That is
  deliberately narrow: `discord_token` for a coordinator, and the three link settings for a node.
  Optional settings — the API port, the whole Lumiere upload path — never trigger it, so an install
  that has run for a year without them is not dragged into a wizard by an upgrade.
- **On demand**, with `pndc --setup`, which asks everything for the detected role and offers the
  current value of each as its default.

The role comes from `--mini` or `pandora_mode` in the config, read directly rather than through
`link::client::is_mini` — that answer is cached for the process, and setup may be about to write
the value that decides it.

## With a terminal

An interview. Each answer is checked for shape offline first — a port that is not a number, a URL
with a trailing slash, a node name containing a space or `|` — and saved as soon as it is accepted,
so an interrupted setup keeps what it already got. Pressing Enter keeps an existing value rather
than clearing it; a secret is never echoed back, only offered as `[keep current]`.

Then each subsystem is verified against the real service:

| Setting | Checked by |
| --- | --- |
| `discord_token` | `GET discord.com/api/v10/users/@me`, reporting the bot's username |
| `lumiere_broker_url` + `lumiere_broker_token` | `GET <broker>/v1/status` |
| the three link settings | an actual `POST /api/v1/link/register` |

Registering is the node check because it is the only call that tests every answer at once: the
coordinator URL, the token, the node name the token is bound to, and whether this build's encoder
matches the coordinator's. A refusal names which of those was wrong — including, for a name
mismatch, the node the token *is* bound to.

A failed check does not stop Pandora starting. It is reported and the run continues, because a
broker that happens to be down is not a reason to refuse to boot.

## Without a terminal

Docker runs `pndc` as a service with no TTY, and `start.sh` runs it in a loop. Blocking on a prompt
there would hang a deploy with no indication of why, so instead setup:

1. prints which required settings are missing, and what each one is for;
2. writes a commented `env.pandora` template (mode `0600`) if none exists — `get_env` already skips
   `#` and blank lines, so the file is valid the moment it is filled in;
3. exits **78** (`EX_CONFIG`).

`start.sh` stops on 78 rather than rebuilding and respinning, since restarting cannot fix a missing
setting.

## What it asks

**Coordinator** — `discord_token` (required), `api_port`, `api_host`, `api_public_url`,
`lumiere_broker_url`, `lumiere_broker_token`, `lumiere_public_url`.

**Node** — `link_coordinator_url`, `link_node_name`, `link_node_token` (all required),
`link_max_jobs`. Choosing the node role also writes `pandora_mode|pntools|mini`.

## Native ffmpeg

Pandora runs whatever `ffmpeg`/`ffprobe` pair `DB/bin` holds, and by default that is a portable
download compiled for the x86-64 baseline so it runs on any machine. `scripts/build-ffmpeg.sh`
compiles one for *this* machine instead: ffmpeg, x264, x265 (8/10/12-bit) and libass from pinned
sources, with `-march=native` on Linux and `-mcpu=native` on Apple silicon, so the compiler uses
every instruction the CPU has — AVX2/FMA/BMI2 on an i9-9900K, NEON and the Apple extensions on an
M-series Mac — in ffmpeg's own code: swscale, the filters, the AAC encoder, the muxers. x264, x265
and libass carry hand-written assembly with runtime dispatch already, so what a native build buys
them is the C around those loops; what it buys ffmpeg is everything.

Three ways to run it, all the same build:

```bash
scripts/build-ffmpeg.sh            # from the checkout; --clean discards the work tree first
pndc --build-ffmpeg [--clean]      # the copy embedded in the binary, for a box with no checkout
/build-ffmpeg [clean:true]         # from Discord, rank 4; the reply shows the step it is on
```

and one way to make startup do it: `ffmpeg_build|pntools|native` in `env.pandora`. With that set,
a startup that finds no pair in `DB/bin` builds one rather than downloading, a portable pair
already there is rebuilt over, and startup waits for the build — which is minutes on a fast
machine and longer on a small node. Without it, nothing changes for an existing deployment.

The script checks its prerequisites first and names the package-manager line that installs the
missing ones (a C/C++ toolchain, make, cmake, pkg-config, curl, xz, git, nasm on x86, and the
development packages for freetype, fontconfig, harfbuzz and fribidi — those four are linked from
the system because fontconfig's configuration belongs to the machine; everything else is built and
linked statically). It smoke-tests the result — a libx264 encode through the `ass` filter and a
10-bit libx265 encode — before installing it, so a failed build leaves the previous pair in place.
It writes `DB/bin/ffmpeg.build` beside the binaries: what was built, with which flags, on which
CPU, and startup prints that so a node's `/lsnode` line and its log agree about what it runs.
Component versions are pinned in the script so two machines that run it get the same ffmpeg,
which is what a Pandora Mini node needs to produce the same frames as its coordinator; bump them
together. NVENC/NVDEC support is compiled in on Linux (header-only, no driver needed to build),
and VAAPI, Intel VPL, dav1d and SVT-AV1 are taken when their development packages are present.

**A native build must not be copied to another machine.** It is tuned to the CPU that built it and
may not start on another; `DB/bin` is per-machine and gitignored for this reason. Windows keeps the
portable download whatever the key says.

Under Docker nothing is installed on the host and nothing has to be typed there either:
`docker-compose.yml` sets `FFMPEG_NATIVE=1` by default, so the image rebuild the gitsync watcher
runs after a `/gitsync` is the whole deployment — the first rebuild after this lands takes the
extra ten-odd minutes the compile costs, later ones hit the cached layer. The build arg runs the
same script in its own image stage — the compiler and the development packages live there and
never reach the host or the runtime image — and the runtime image then carries that pair on PATH
in place of Debian's `ffmpeg` package, with the record at `/usr/local/share/pandora/ffmpeg.build`
so startup reports it. Build the image on the machine that runs it: the pair is tuned to the CPU
that built it. The layer is cached on the script's content, so a `/gitsync`-triggered rebuild only
recompiles ffmpeg when `scripts/build-ffmpeg.sh` changed; `FFMPEG_NATIVE=0` in the compose `.env`
opts out. If the ffmpeg stage fails, the image build fails and the watcher's `up` brings the
previous image back, so the bot returns on the old code rather than not at all — a `/gitsync`
whose commits do not show up is the symptom, and the watcher's console has the compiler output. `FFMPEG_TOOLCHAIN=1` is the separate, optional arg that adds the
compiler to the runtime image so `docker compose exec pndc pndc --build-ffmpeg` or `/build-ffmpeg`
can rebuild into the mounted `./DB/bin` without an image rebuild; `DB/bin` wins over PATH when both
hold a pair. Without that arg the runtime image has no compiler, and both `/build-ffmpeg` and
`pndc --build-ffmpeg` say so up front instead of running the script into its prerequisite check —
`/build-ffmpeg` also says whether the ffmpeg in use is already the image's native pair, which with
the compose default it is.

## Migrations on a new install

A new install records every [migration](LINK.md#migrations) as already run, without running any. It
is by definition in the current on-disk format — setup is what just wrote it — so there is nothing
for a migration to convert.

The signal is the absence of `env.pandora` when `ensure_configured()` starts, which is the one
unambiguous mark of a machine that has never run Pandora. A deployment that predates the ledger has
no such guarantee, reaches its first `/gitsync` with no ledger at all, and runs every migration from
zero — which is the case the ledger exists for.

Everything beyond this stays where it already lives: per-server settings in `/configure` and
`/edit`, provider credentials in [PROJECT.md](PROJECT.md), the node's own story in [LINK.md](LINK.md).
