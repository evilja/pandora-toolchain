# LINK.md

Pandora Mini: a Discord-less Pandora node that leases whole jobs from a coordinating `pndc`,
fetches their sources itself, runs them through the ordinary worker runtime, and reports back.

A coordinator with no registered nodes behaves exactly as it did before the link existed. Nothing
below changes how a local job runs.

## Shape

A node takes **whole jobs**, not parts of one. It fetches its own source and does its own upload, so
what crosses the link is a job spec of a few KB plus a progress stream — the 5–10 GB source and the
release itself never touch the coordinator. Because an episode is encoded entirely on one machine,
there is no cross-machine consistency requirement inside a release.

Nodes have **no inbound surface**. Every exchange starts as an outbound request from the node, so a
node needs no public hostname, no tunnel and no open port; the coordinator's existing
`api_public_url` serves the whole cluster, and only one direction of authentication exists.

The coordinator stays the system of record: the `jobs` row, the Discord message and web console, the
publish commands, the Drive deletion capabilities, and the archived logs. A remote job is a local
`Job` mirroring a remote execution, and it renders through exactly the same path as any other — see
[Reporting](#reporting).

## Running a node

```bash
pndc --mini            # or `pandora_mode|pntools|mini` in env.pandora
```

Mini mode runs the same startup as a coordinator — config migration, `ensure_startup_binaries`
(which bootstraps portable ffmpeg/ffprobe into `DB/bin`), the font cache, localisation — creates the
same worker queue, spawns `pn_worker`, and then starts the link client **instead of** the Discord
client. No `discord_token` is needed, and **no HTTP API is served**: a node has no inbound surface
by design, and its `env.pandora` is very often a copy of the coordinator's with the link keys added,
which used to mean it quietly served the whole API on whatever `api_port` it inherited. An
`api_port` on a node is noted at startup and ignored. Every job a node runs uses `Frontend::None`, which the worker
pipeline already treats as a no-op for every message edit, reaction and presence update.

A node keeps its own sqlite database because `pn_worker` needs one. It is scratch, not a record:
`fail_stale_active()` at startup means a restarted node never resumes a job, matching the
deliberately non-resuming reboot policy in [WORKER.md](WORKER.md).

### What the working directory has to be

The process's current directory is the node's whole world, and three separate things read it. Two of
them are silent when it is wrong.

- **It holds `DB/`** — the config, the token, the scratch database, the synced fonts, and
  `build.pandora`. All of those paths are relative.
- **It must be a git checkout of this repository.** `release::repo_path()` is the process's cwd
  unless `PANDORA_GITSYNC_REPO` says otherwise, so a node run from a bare directory pulls nothing
  and logs `could not find repository at <cwd>` on every update attempt. It keeps taking work — see
  [Staying level](#staying-level) — so this reads as a node that is merely stuck a build behind.
  Point `PANDORA_GITSYNC_REPO` at a *separate* checkout rather than a working one if you split them:
  the pull ends in a forced `checkout_head`, which discards uncommitted changes.
- **It needs a restart loop around it.** There is no in-place upgrade — the binary that pulled the
  source is the old one — so `restart_into_new_build` records the build, exits, and expects
  something to rebuild and relaunch. `start.sh` is that loop for a coordinator; a node needs the
  same shape around `pndc --mini`. Without one, a successful update stops the node. A node that
  cannot read its own link configuration exits `78`/`EX_CONFIG`, which `start.sh` stops on rather
  than respins — returning normally would have ended the process successfully and had the loop
  restart it forever.

The loop is also where the **build environment** lives, and it is the one part of this that fails
loudly. A node's `encoder_identity` is compared against the coordinator's and a mismatch is refused
outright, so a rebuild that cannot find the forked libx264 — `PNX264_LIB_DIR`, `PNX264_INCLUDE_DIR`
and `PNX264_STATIC`, read by `pnx264/build.rs` — links the distro's, comes back as `-stock`, and is
turned away. Exporting those in the loop rather than in a shell profile is what makes an unattended
restart reproduce the binary the node registered with.

## Orchestrator mode

```bash
pndc --orchestrator     # or `pandora_mode|pntools|orchestrator` in env.pandora
```

A coordinator that **downloads no video and runs no encoder**. It is an ordinary coordinator in
every other respect — the Discord client, the HTTP API, the queue, the roster, the publish commands,
the Drive capabilities and the archived logs are all exactly as they are without the flag — and it
differs in one rule: **every job a node can run is held for a node, and nothing that would fetch or
encode a release here is accepted at all.**

`--mini` and `--orchestrator` are opposites and `pndc` refuses to start with both. The mode implies
`link_enabled`: everywhere else that key being off means "run everything locally", and here it would
mean nothing runs at all, with no error and no node to look at.

### What waits, and what is refused

- **Held for a node.** Everything in [What can be leased](#what-can-be-leased): `Encode`,
  `Pancode`, `Backup`, `Probe`, and batch children. `must_offload` is exactly "leasable", so there
  is no second rule to keep in step with the first. A probe waits with the rest — an orchestrator
  fetches not even the metainfo — which means `/job` shows "waiting for a node" before it shows a
  file list.
- **Refused at submission**, with the reason, because there is no node to send them to and nothing
  here that may run them:
  - *Their input is a file that only exists here.* Keeps (a kept encode leaves its output for a
    later `/keycode` to join), `/keycode` itself, `/preview`, and Studio.
  - *They download the release themselves.* `/backupall`, and `/subs` — extracting a subtitle track
    means fetching the video it is in.

  The alternative to refusing is a job that is accepted, renders a queue position, and then either
  sits forever or quietly does the thing the deployment exists not to do.
- **Neither.** Cancels, `/workers`, `/lsnode`, the git commands, the publish commands and every
  config command are unchanged: none of them touches a video.

### A batch parent stops downloading

A batch parent's whole job is normally one download feeding children that hard-link out of it, and
on an orchestrator it does not run at all. Instead **every entry is claimed into a leased child as
soon as the file list exists** — not only when a node happens to be free — and each node fetches its
own episode. The parent stays at `Queued`, holds the metainfo its children travel with, and ends
when the last episode does.

This also retires the cost described under [Batch split](#batch-split): with no parent download
there is no episode fetched twice, and cluster-wide download is 1×. `settle_download` never fires,
which is right — there is no delivery here that could come up short. An episode that can never be
placed at all is counted against the batch rather than left to block every episode behind it.

### Waiting

A held job is `Queued`, wears the worker `lnk-wait`, and is skipped by every local dispatch exactly
as a leased job is. `/workers` counts it as waiting rather than active, because nothing is running
it anywhere.

**It waits with a reason.** `pick_node` records which filter emptied the candidate list — every node
is draining, no node is marked for GPU work, no node has proved the encoder this preset needs, every
free node is reserved for another server — and the job's message and the worker snapshot
(`link_wait_reason`) carry it. This is the one failure mode the flag could introduce that the
ordinary coordinator does not have: a job that simply does not start. The reason is what makes it a
thing somebody can act on, and it is re-rendered when it changes rather than on every pass.

Offers are retried on every loop pass, and the question is a lookup over the in-memory roster: no
job waiting for a node reads a disk until a node is actually free for it.

**A node this job has already been turned away from is stepped over.** Both a decline and a lost
lease put the node on that list, because handing the job straight back to the machine that just
failed it is how one unhealthy box becomes a job that never runs anywhere. The exclusion expires
after five minutes — everything that puts a node on the list is something that gets fixed, and an
operator who fixes a node should not also have to resubmit what met it while it was broken.

### When it ends

`LINK_MAX_ATTEMPTS` still bounds the tour, and past it there is nowhere to fall back to, so the job
**fails** naming the attempts and the nodes it was lost on. A queue entry that never moves would be
the worse outcome: this way the failure is visible, in the job's own message, where the person who
submitted it is looking.

A batch behaves the same way per episode. Every entry is claimed into a waiting child as soon as the
parent has its file list — not only when a node happens to be free, which is what stops
`settle_download` from counting a still-unclaimed episode as one the torrent never delivered — and
an episode that can never be placed is counted against the batch rather than left to block every
episode behind it.

## Configuration

Node side:

| Key | Meaning |
| --- | --- |
| `pandora_mode` | `mini` enables link mode (equivalently `--mini`) |
| `link_coordinator_url` | coordinator origin, no trailing slash |
| `link_node_token` | this node's `\|link\|` token |
| `link_node_name` | stable node identity; must match the token's node name |
| `link_max_jobs` | concurrent leases, default `1` |
| `link_auto_update` | keep level with the coordinator's build, default on. See [Staying level](#staying-level) |

Coordinator side:

| Key | Meaning |
| --- | --- |
| `link_enabled` | master switch for offload; without it nothing is ever leased |
| `link_only_node` | limit all offload to one named node |
| `link_lease_timeout_secs` | how long a silent lease is kept, default `90` |
| `link_allow_build_mismatch` | permit a node whose pnmpeg differs from the coordinator's |
| `pandora_mode` | `orchestrator` holds every encode for a node and runs none here (equivalently `--orchestrator`). See [Orchestrator mode](#orchestrator-mode) |

State:

- `DB/config/global/environment/link_nodes.json` (coordinator, mode `0600`) — the node roster as a
  JSON array of `{ name, pandora_version, encoder_identity, ffmpeg_version, threads,
  max_jobs, encoders, build, migration_error, registered_at, last_seen, drain, group }`. The roster is
  advisory: a node registers on startup and again every thirty seconds. It is persisted for the three
  things a restart must not forget — an operator's drain flag, their `/teenode` grouping, and their
  `/limit` reservation (`reserved_for`, the guild id a node works for and nothing else). `purpose` is
  deliberately *not* persisted: it belongs to the token, and a value carried across a restart would
  outlive the token that justified it.
- `DB/config/global/environment/build.pandora` (both sides) — the build this machine is level with
  and the commit it was recorded for. See [Staying level](#staying-level).
- `DB/config/global/environment/migration.pandora` (both sides) — the highest migration id that has
  run here, and the one that stopped the last run if any. See [Migrations](#migrations).

## Tokens

Node tokens live in the same `api.pandora` as every other token, as `<token>|link|<node>|<purpose>`.
Mint one with `/gentoken link:<node> purpose:<cpu|gpu|both>` (see [DISCORD.md](DISCORD.md));
`local` and `link` are mutually exclusive, and a node name may contain neither whitespace nor `|`.

The fourth field is what a node is **for**, and it is the whole of the CPU/GPU distinction — see
[Purpose](#purpose). It is absent on every token minted before the field existed, and an absent
field means `cpu`; the `1788177600-link-token-purpose` migration fills it in from each token's
label, which is where operators used to write it by hand.

A link token opens **only** the `/api/v1/link/*` routes and nothing else — it cannot submit jobs,
read logs, or reach git. Logs and git are refused by `require_privileged` and `require_local`, which
a node token satisfies neither of; submission is refused in `submit_with_progress`, the one function
every submit route funnels through — without that a node token would have reached `Reach::Own` like
any plain one, and `effective_server_id` would have honoured whichever `server_id` the body named. Every link route additionally checks that the node named in the request
body matches the node the token is bound to, so one node cannot renew or finish another's lease.
Link traffic is exempt from the API write rate limit: a node renews every lease every ten seconds
for as long as it works, which is a heartbeat on a fixed cadence rather than user traffic.

## Routes

All under `/api/v1/link/`, all requiring a link token.

- `POST /link/register` — the node announces itself (name, version, `encoder_identity`, thread
  count, `max_jobs`, hardware `encoders` proved by real test encodes, the `build` it is level with,
  and any `migration_error`). Returns
  `{ accepted, reason?, renew_secs, lease_timeout_secs, assets_revision, purpose, release, drain }`.
  A node repeats it every thirty seconds for as long as it runs — see [Saying hello
  again](#saying-hello-again).
- `GET /link/lease?node=<name>` — **long poll**, up to 30s. Returns a job spec, or `204` when there
  is nothing waiting. This is the only dispatch mechanism.
- `POST /link/lease/:id/renew` — heartbeat plus the node's worker output. Returns
  `{ cancel, abandon, drain }`.
- `POST /link/lease/:id/result` — the terminal report. `409` when the lease is already gone or
  belongs to another node, which tells the node to stop retrying without looking like a transport
  fault.
- `PUT /link/lease/:id/output` — a finished encode coming back for local publication. Streamed
  straight to disk and accepted only from the node that holds the lease. See
  [HLS and returned output](#hls-and-returned-output).
- `GET /link/release` — what the coordinator is running: `{ version, build, commit, reset }`. A node
  polls it once per loop pass. See [Staying level](#staying-level).
- `GET /link/assets/manifest` — the font and intro corpus, with its revision. See [Assets](#assets).
- `GET /link/assets/:hash` — one asset, addressed by content hash.

## Saying hello again

A node registers on startup and then **every thirty seconds**, whether or not it is working. The
register answer is the only channel an *idle* node has: `GET /link/lease` long-polls and returns a
bare `204`, and the `drain` and `assets_revision` on a renew answer only reach a node that is
holding a lease.

Three things depend on the repeat, and all three were silently wrong when registration happened
once:

- **`purpose` is not persisted.** It belongs to the token, so the roster deliberately does not keep
  it across a restart — which means a restarted coordinator reloads every node as `cpu`. Because
  the lease long-poll refreshes `last_seen`, such a node looks perfectly healthy while a box marked
  `gpu` is offered the general encoding it was marked to keep off, and is never offered GPU work
  again however long its proved `encoders` list is.
- **Un-draining an idle node.** `/drainnode name:<node> drain:false` puts the node back in
  `pick_node`, and the coordinator starts offering it work; without the register the node's own
  copy of the flag stays latched at whatever the last lease it held was told, so it never polls,
  every offer expires uncollected after sixty seconds, and each expiry spends one of the job's
  `LINK_MAX_ATTEMPTS`. The refresh interval is under that pickup window on purpose.
- **`/rmnode`.** It clears a roster entry; the node puts itself back within half a minute. That is
  what the command is for — it forgets stale state, including a drain flag, a `/teenode` group and
  a `/limit` reservation. It is not how a node is turned off; `/drainnode` is, and revoking the
  token with `/rmtoken` is how one is removed for good.

A refused registration is treated as a drain: the node finishes what it holds, takes nothing new,
and keeps saying hello until it is accepted. A register that fails to *reach* the coordinator
changes nothing — a blip is not an instruction, and guessing would drain a cluster over one.

## Encoder parity

`register` carries an `encoder_identity`: the libx264 the node encodes with, as
`x264-<build>-<pointver>-<pandora|stock>` — for the pinned fork, `x264-165-0.165.x-pandora`. That,
and nothing else about the binary, is what decides an encode. A node on a different distribution,
with a different Rust compiler or libc, produces identical frames as long as this matches.

The coordinator **refuses a mismatch by default**, and refuses a node that reports no identity at
all. An episode is encoded entirely on one machine, so a mismatch cannot corrupt a file; but two
x264 builds make different rate decisions at the same CRF, and a cluster that quietly ships two
quality tiers is not worth debugging later. Treating an absent identity as "no opinion" would have
made the check optional for anyone who omitted the field.

`link_allow_build_mismatch` disables both refusals. It is the escape hatch for a cluster that
knowingly runs different encoders; it is not a way to work around a build that merely *looks*
different.

This deliberately replaced hashing the pnmpeg binary. A hash covers the whole toolchain, so it
refuses nodes that are genuinely encoder-equivalent — a build on another distribution never
matches — which leaves an operator no option but to turn the check off entirely, including for the
case it exists to catch.

### Building a node against the fork

`pnx264/build.rs` finds libx264 through `PNX264_INCLUDE_DIR`, `PNX264_LIB_DIR` and
`PNX264_STATIC=1`; without them it links whatever the linker already sees, which is the distro
x264 and reports itself as `-stock`. A node meant to join a Pandora cluster wants the fork:

```bash
export PNX264_INCLUDE_DIR=/path/to/x264-pandora/include
export PNX264_LIB_DIR=/path/to/x264-pandora/lib
export PNX264_STATIC=1
cargo build --release
```

The prebuilt fork release is laid out exactly as those variables expect (`include/x264.h`,
`lib/libx264.a`). Verify its SHA-256 before use, and check `#define X264_PANDORA_PLAN_ONLY 1` is
present in the header — plan-only mode, which the VerySlow parallel planner needs, exists only in
the fork. The Docker image builds the same fork from pinned source instead; see `Dockerfile`.

## Purpose

A node is a **CPU node**, a **GPU node**, or **both**, and that decides which presets it is ever
offered. Every preset declares the hardware it needs (`hardware = "gpu"` in its file, or the
built-in table's answer); `pick_node` refuses a node whose purpose does not accept it.

The reason it is a hard filter rather than a preference is what a GPU preset does on a machine
without one: ffmpeg either refuses the encoder outright — a failed job and a wasted lease — or
falls back to a software encoder and ships a release at a quality tier nobody chose. The second
outcome is the dangerous one, because nothing about it looks like a failure.

**The purpose comes from the node's token, never from the node.** A machine reporting its own
capabilities could put itself in the way of work it cannot run, by misconfiguration as easily as by
intent; a token is minted by an operator who knows what the box is. Changing a node's purpose means
minting a new token, not editing a config file on the node.

**A token that names no purpose is `cpu`.** That is an answer rather than a fallback: the machines
that predate the field are the CPU boxes the cluster was built out of, and reading an unmarked
token as "anything" would send the first GPU preset to one of them.

`both` exists for a machine that genuinely serves both, and is the only way to say so. A `gpu` node
is not a fallback for CPU work — it was marked `gpu` to keep general encoding off it.

Purpose is necessary but not sufficient for GPU work. After the token tells a node it is `gpu` or
`both`, it runs a half-second `testsrc2` encode through each known AMF/NVENC/QSV/VAAPI backend and
re-registers only the encoders that produced output. The coordinator resolves the preset's video
codec and requires it in that list. `ffmpeg -encoders` is deliberately not used: it reports what
the binary was built with, not what this card and driver can open. An empty list grants no GPU
capability, so an older node cannot accept AV1 merely because its token says `gpu`.

## Staying level

Every machine in a cluster runs the same source. A node that does not is not merely out of date:
its encoder settings, its preset table and its half of the wire format are all compiled in, and the
cluster is agreeing on values it no longer holds.

The coordinator advertises a **build**: a counter in `build.pandora`, bumped by every `/gitsync`
that moved HEAD, together with the commit it was bumped for. Version alone cannot serve — it
changes when somebody edits `Cargo.toml`, not when a deploy happens. The number is persisted
because a gitsync ends in `exit(0)`: it has to survive the restart it causes and be correct by the
time the API answers again.

A node polls `GET /link/release` once per loop pass and compares. It is a poll rather than a field
on an existing answer because of which call an idle node makes: `GET /link/lease` long-polls and
returns `204` with no body, so a node with no work would learn nothing until it took a job — which
is exactly the moment not to discover it needs to restart. `register` carries the same information
for the first check, and the poll carries every one after.

On a mismatch the node **drains**: it takes nothing new, finishes what it holds, and only then
pulls. A restart mid-encode throws away the encode, and the encode is the expensive thing here.
Then it runs [migrations](#migrations), records the coordinator's build number, and exits into its
own restart loop, which rebuilds before it comes back.

**A node that pulls and still does not land on the coordinator's commit does not restart.** It logs
why, waits ten minutes, and goes back to taking work in the meantime. The failures that reach this
point are repository problems — a diverged branch, a credential that stopped working — and none of
them resolve in seconds; a node that restarted anyway would rebuild the same source, record
nothing, and arrive back here, which across a cluster is a restart loop. Running one build behind
is the cheaper failure, and `/lsnode` shows the build that stopped moving.

`link_auto_update` turns the pull and the restart off for a node whose checkout somebody else
manages. It does not turn off the comparison: the node still reports its build, so it shows as
sitting behind rather than not showing at all.

### `/gitforce`

`/gitsync` fast-forwards, and bumps the build only when HEAD actually moved. That is what makes it
safe to run constantly: a sync that pulled nothing does not drain and restart the whole cluster to
arrive back where it started.

`/gitforce` is the other lever. It **resets** the coordinator onto origin's tip rather than
fast-forwarding towards it, bumps the build whether or not anything moved, and sets `reset` on the
advertised release so every node resets onto the same commit too. It exists for the two cases a
fast-forward cannot serve — a checkout that has diverged, and a rebuild that has to reach the
cluster without a new commit — and it is a separate command precisely so that cost is asked for
rather than paid by accident. **It discards local working-tree changes, here and on every node.**

The forced flag is recorded against the build it belongs to, so the next ordinary `/gitsync` bumps
past it and the reset stops applying. A node satisfies a forced release by having *recorded* that
build, not by holding the right commit — the point of a reset is a checkout that may be dirty in
ways HEAD does not show.

## Migrations

On-disk changes a new revision needs, kept out of the Rust that would otherwise carry them forever.
A migration is a pair of scripts in `migration/` at the repository root — one `.sh`, one `.ps1`, so
either platform can deploy it — and both `/gitsync` and a node's self-update run whichever half
this platform can, **after the pull and before the restart**. That is the only moment they can run:
the scripts are the newly pulled ones while the binary is still the old one, which is exactly the
order a migration needs, since it prepares the state the build about to be compiled expects.

Ordering is by an id in a header comment:

```sh
#!/usr/bin/env sh
# pandora-migration: 1788177600
```

The value is a unix time only so that two people writing migrations on the same day cannot pick the
same number. **Nothing ever compares it against the deployed machine's clock.** It is an identifier
that goes forward, and the only comparison is against the highest one this machine has already run,
which lives in `migration.pandora`. A file with no header is not a migration — a README, a helper
the scripts source — and is skipped rather than guessed at.

Scripts run from the process's working directory, not from the repository: they operate on `DB/`,
which under Docker is beside the binary and not inside the checkout at all. The repository reaches
them as `PANDORA_REPO`. The ledger advances per script, so a run that dies halfway keeps what it
achieved, and a failure leaves the ledger *below* the script that failed — which is what makes the
next sync retry it.

**A failed migration does not stop the restart.** Refusing to restart would strand the machine on
an old binary with new source checked out, which is worse than the thing that failed. Instead the
reason is recorded, and a node reports it to the coordinator on its next register, where `/lsnode`
shows it under the node it belongs to — otherwise the one machine that failed to migrate would also
be the one machine nobody hears from about it.

**A new install records every migration as done without running any.** It is already in the current
format — `pndc --setup` just wrote it — and there is nothing to convert. The signal is the absence
of `env.pandora` at startup, which is the one unambiguous mark of a machine that has never run
Pandora; a deployment that predates the ledger has no such guarantee and runs everything from zero.

## Assets

Fonts and intro videos are the two things a node needs that do not travel in a job spec, and a
missing font does not fail — libass substitutes one and the release goes out in the wrong typeface
with nothing to show for it. So a node syncs the coordinator's whole corpus, and a job whose
revision it cannot prove it holds is declined rather than encoded.

The corpus is compared **by content**, never by name or timestamp: two machines agree when their
files hash the same, which is the only definition that survives a copy, a re-download, or a
filesystem that rounds mtimes. The revision is a hash over the whole entry list, so it changes
exactly when the corpus does and there is no counter for `/cfont` to remember to bump.

- Fonts land in `DB/fontconfig/<bucket>`, which is where the startup installer already copies from
  into the OS font path — libass resolves through system fontconfig, so a synced font is not a
  usable font until that has run.
- Intros land in `DB/cache/link-intros/<group>`, deliberately apart from anything an operator
  hand-placed, because pnmpeg writes compatibility variants back into whatever folder it is given.
  A spec carries the *group name*, never the coordinator's folder path, which means nothing on
  another machine.

**Deletions are pruned, and only for intros.** Fetching what is missing cannot see a file being
removed: the revision moves, nothing is missing, and a node would record the new revision while
still holding what the corpus dropped — so two machines agreeing on a revision would not mean they
held the same corpus. That is not cosmetic for an intro, because the whole folder is handed to
pnmpeg and it picks a variant out of it: a retired intro would go on shipping from every node that
ever had it. Fonts are left alone on purpose — `DB/fontconfig` is shared with fonts an operator
placed by hand, the installer has already copied them somewhere deleting the bucket copy would not
reach, and an extra font substitutes for nothing.

pnmpeg's own `pnmpeg_compat_*` variants are **not** part of the corpus. They are a per-machine cache
derived from it, keyed by one episode's exact stream properties; they appear in the coordinator's
intro folder as it encodes, and counting them would move the revision — and re-sync every node —
every time an unfamiliar output format was met. A node regenerates its own, and a prune drops them
along with everything else, since they were derived from a set that has just changed.

## What can be leased

The rule is "does the job carry its own source", since a node fetches its own input:

- **Leasable**: `Encode`, `Pancode`, `Backup`, `Probe`, and **batch children**, provided the job
  carries a source the node can fetch — a non-empty link, or a `.torrent` the coordinator sends with
  it (see below).
- **Never leased**: forwarded jobs (they mirror another job's outcome and run nowhere); batch
  *parents* (a parent is one torrent download feeding many children that hard-link out of it, so
  leasing it would put the download on a node and the encodes here); keeps, `Keycode`, `Preview`
  and `Studio` (their inputs are files on the coordinator); and any job past `LINK_MAX_ATTEMPTS`.

A leased Pancode carries its originating `probe_job_id` alongside the source link. The node has no
probe job and will fail to adopt its saved `.torrent`; the link is what the download falls back to,
and the file index selects the episode.

A lease naming a preset that exists only as a file needs that file on the node too: the node resolves
the spec's preset through its own `preset_from_name`, which reaches its own
`DB/config/global/presets/`. Preset files are not part of the synced asset corpus, so a node without
one declines the lease with `unsupported preset <name>` and the coordinator runs the job itself.

### Sending the `.torrent` itself

A job whose only route to its input was the metainfo file its probe saved on the coordinator used to
be refused a node for having no fetchable source. `LinkJobSpec.torrent_b64` carries those bytes, and
the node writes them to its own `contents/fetch.torrent` before the job is queued. That is not a new
download path: `TorrentType::Link("")` with a `fetch.torrent` present is the existing shape for a
torrent that is already local, so nothing downstream can tell a metainfo that was handed over from
one that was fetched. The bytes are sent only when the source link is empty — a link or a magnet is
something the node resolves for itself, and metainfo can be hundreds of kilobytes of lease payload.

### Batch split

Batch episodes are offered to nodes **one file index at a time**, by `do_batch_lease_things` on
every pass of the worker loop. A child is a Pancode carrying its parent's torrent and its own file
index, which is the shape a leased Pancode already travelled in; the node fetches that one episode
itself. The offer happens *before* the parent has downloaded that file, because that is the only
moment leasing it is worth anything — afterwards the bytes are already here.

**A leased episode is fetched twice** — unless this coordinator is an
[orchestrator](#orchestrator-mode), where the parent does not download at all and every episode is
leased. Otherwise the parent keeps its whole selection: `FileSelection` becomes
a piece bitmap when the session opens and there is no way to narrow a running download, so the
parent still pulls a file it will not use. This trades the coordinator's bandwidth for the cluster's
encoders, on the reasoning that a batch is bounded by encoding rather than by a download it had to
do anyway.

Three things keep the two paths from colliding over one episode:

- **`entry.job_id` is the claim.** Offering sets it, and `spawn_batch_child` returns early on a
  claimed entry when the parent's own copy lands, so an episode is never encoded twice.
- **`settle_download` counts only unclaimed entries** as never delivered, so a leased episode is not
  also counted as a file the torrent failed to produce.
- **A batch child never archives its parent's probe.** Every sibling still needs the `.torrent` it
  saved. Local children avoid this by carrying no `probe_job_id`; a leased one must carry it (the
  node's `queue_pancode_job` refuses a Pancode that names no probe, and a lost lease comes back
  through that same path), so the refusal lives in `finish_link_job` and in the worker-message
  archival instead.

A lease that is lost or declined returns through `requeue_link_job` and the episode is encoded here.
If the local path refuses it outright, the batch is told — otherwise a parent would wait forever for
a child that no longer exists anywhere.

## Upload policy and returned output

A node holds no `meta.pandora` for the guild a job came from, so the server's upload policy travels
with the job rather than being looked up on the far side. Without that a node would publish to
streaming hosts a server had deliberately switched off.

- **`drive_only`** is resolved by the coordinator and carried in the spec. The node's upload worker
  takes it as an override; every local job passes `None` and reads the server file exactly as
  before.
- **`server_id`** is carried too, so Drive uploads land under the originating guild's Lumiere
  profile rather than the global one.

### HLS and returned output

An HLS release is served for twelve hours from `/lumiere/v1/hls/<capability>` on the machine that
published it, and a node has no public hostname. Such jobs are still **encoded** remotely; only the
publishing stays here.

The coordinator sets `return_output` on the spec. The node encodes to an ordinary MP4 — it never
holds the server's HLS setting, so it produces no HLS layout of its own — and then **stops at
`Encoded`** rather than uploading: the output is not its to publish. Its link client `PUT`s the file
to `/link/lease/:id/output`, the node's worker loop releases the work directory once the file is
gone, and the client reports `returned`.

A probe is the other outcome that does not end its job. `Probed` is where a probe stops locally
too — it then waits for a file to be selected, and the probe timeout archives it — so the node
reports `probed`, the coordinator releases the lease and clears `link_node`, and the job stays in
the queue exactly where a local probe would leave it.

That selection window runs from when the file list *appeared*, not from when the job was submitted.
Measured from the submission, a probe that took longer than the window to answer had its answer
deleted the instant it arrived — the message somebody was waiting for, gone before they could read
it, with nothing anywhere saying why. That was always reachable behind a long queue; on an
[orchestrator](#orchestrator-mode), where a probe spends the wait held for a node, it is ordinary. Without that the lease would simply expire
and the coordinator would re-run a probe that had already answered.

`returned` is not a terminal outcome. The coordinator clears `link_node`, leaves the job at
`Encoded`, and the ordinary local pipeline dispatches the upload — so the HLS publication, its
capability and its playback URL are all produced here, on the hostname that is already public.

The order on the node is deliberate: send the file, release the job, then report. A report that
never lands costs only a requeue; a work directory wiped before the file was sent costs the encode.
If the coordinator is told an output was returned and finds nothing on disk, the job **fails** rather
than requeueing — the machine that just failed to deliver is not worth a second whole encode.

This is the only large body the link carries. It is streamed to disk on arrival rather than
buffered, written beside its target and renamed, and accepted only from the node that holds the
lease.

## Scheduling

Offload is automatic and non-blocking. A job is offered to a node when one is registered, alive,
undrained, under its `max_jobs`, marked for the hardware the job's preset needs (see
[Purpose](#purpose)), and, for GPU work, advertising the encoder codec the preset names; otherwise the job runs locally exactly as
before. **A job never waits for a node** — the cluster being full, drained or absent is
never a reason for work to sit still. [Orchestrator mode](#orchestrator-mode) is the one deployment
where it is, and it is a flag precisely because that is the opposite trade.

Nodes are ranked most-idle first, then by thread count, so a cluster of unequal machines fills its
biggest free box before its smallest. `link_only_node` limits offload to a single named node, which
is how a node is trialled or a misbehaving one bypassed without deleting its token.

### Reserving a node for one server

`/limit name:<node>` reserves a node for the guild the command is used in: `pick_node` offers it
nothing from any other server, and those jobs run on the coordinator or on another free node exactly
as they would have. `/limit name:<node> clear:true` releases it.

**The rule is one-way.** Reserving a node narrows who may use it and says nothing about which nodes
that server uses — it keeps every other node it could already reach. A job carrying no guild at all
is not offered a reserved node either: `None` is not the server it was kept for.

The reservation lives on the coordinator's roster, not in the node's own config, because a node must
not be able to decide who it serves — a contributed box could otherwise be redirected by editing a
file on it. It is persisted for the same reason `drain` and `group` are, survives the node
re-registering, and does not interrupt a lease the node is already running. There is no field for
naming a different guild: a hand-typed id is one nobody can check, and a node reserved to the wrong
server simply stops taking work with no error anywhere. `/lsnode` prints a `🔒` line under any
reserved node, saying "this server" or naming the other guild's id.

Leases are **targeted before the node polls**: `pn_worker` picks the node and creates the lease, and
`GET /link/lease` hands over only what is waiting under that node's name. A second node polling can
never take work meant for the first.

## Reporting

A node forwards the **payload** its own workers produced — the message id and its arguments, plus
the stage transition it carried — rather than a summary the coordinator would have to invert. The
coordinator replays those through `persist_side_effects` and `render`, so a remote job needs no
rendering path of its own: the web console gets the same progress JSON, the Drive helpers keep their
deletion capability and Smartcode pointer, and the message is localised against the **job's** own
language rather than the node's.

The tap is `lifecycle::render`, not the `CommData` stream, because declines and cancellations never
reach that stream — hooking the message bus would have reported encode progress faithfully and lost
the reason a job was refused.

Message ids are validated against the built-in locale table, so an id is acceptable exactly when it
has a translation. A node running a newer build that names a message this one lacks is reported and
skipped rather than rendered as the wrong text.

Repeated ticks of the same message id coalesce on the node, so an encode progress bar does not grow
a buffer between renews while distinct events are always kept.

The worker label a leased job wears is `lnk-<node>`, which is what `/workers` and the job embed
render — that is where an operator finds out which machine has their episode. `worker_waiting`
does not list it, so a leased job correctly counts as active.

### Grouping

`/teenode name:<node> group:<name>` (rank 4) sets a node's shared display name, and `worker_label`
then builds `lnk-<group>` instead of `lnk-<node>`. A farm of interchangeable machines is one worker
in the job embed and one row on the console, rather than as many hostnames as there are boxes.

**Only the label merges.** The roster keeps one entry per machine, each node registers, is offered
work, leases, renews and finishes under its own name, and `/drainnode` or `/rmnode` on a grouped
node still means that one machine. Nothing about scheduling reads the group. Grouping is one lookup
in `board::display_name`, which is why `worker_label` is the only place that had to know about it —
and why `/lsnode` and the worker snapshot show the group *beside* the node rather than instead of
it: a stall has to remain traceable to the box that is stalling. Passing `-`, or omitting `group`,
ungroups. The name may contain no whitespace, control character or `|`, and is capped at 32
characters, because it lands in a job row's `worker` column.

## Log shipping

Shipping incrementally rather than bundling at the end is the point: a log that only arrives when a
job ends is no use for the case job logs exist for, which is a job that is stuck and has not ended
at all. A node sends whatever each of its logs has gained since the last renew, and the coordinator
appends it into that job's own log directory.

- **Offsets, not appends.** Each chunk carries the position it belongs at. A chunk the node re-sent
  after a renew it never saw succeed lands entirely behind what is already written and is skipped,
  so a repeat is harmless rather than a duplicated block in the middle of a transcript.
- **Offsets advance on success only**, so a failed renew costs a repeat rather than a hole.
- **A gap is recorded, not hidden.** If bytes genuinely never arrived, the transcript says so
  where they should have been — two disjoint halves spliced together read as one continuous log and
  are a lie about what the tool printed.
- **A shorter file is a new one.** A retry of the same job writes a fresh log from zero; that
  arrives as a reset and replaces the previous attempt's rather than splicing onto it.
- **A terminal job flushes first.** Before the result is sent — and with it the lease, and with the
  lease the only channel these logs have — the node ships everything remaining. What the tools wrote
  in their last seconds is exactly the part worth reading.
- **Bounds.** 256 KiB per file per renew, which is far more than a throttled encoder log uses; a
  file that outruns it catches up over the renews that follow. On the coordinator, 64 MiB per file,
  after which it stops growing and says so once. The name is validated as a plain file name, since
  it arrives off the wire and becomes a path component.

Logs are shipped as text with lossy UTF-8 conversion: ffmpeg occasionally emits a byte that is not
valid UTF-8, and a replacement character in a transcript beats refusing to ship the transcript.

Because the logs land in the coordinator's own directory, `cleanup_job` carries them into
`DB/saved_data/<job>/log` when the job archives, exactly as it does for a local job.

## Failure handling

A remote job is cheap to lose: its inputs are a link and a few KB of subtitle, so **requeue, not
recovery**, is the response to every node failure.

- **Lease expiry.** No renew for `link_lease_timeout_secs` (default 90) and the coordinator reclaims
  the job, returning it to the queue as an ordinary local candidate. An offered lease nobody
  collects is given a shorter rope — 60 seconds — because nothing has started, and for the same
  reason it **spends no attempt**: nothing ran it, which is the answer a decline already gets.
- **An offer is re-checked before it is made.** `pick_node` answers several awaits before the lease
  is created — the server's upload policy and the work directory are resolved in between, and every
  link route writes the board meanwhile — so the node is asked about again inside the lock that
  creates the lease. A node that has since drained, filled up or been removed takes nothing; the job
  runs here at once (or is offered elsewhere) instead of losing a minute to a pickup window nobody
  was going to answer.
- **A node that never collects is drained.** Three offers in a row that expire uncollected and the
  coordinator drains the node itself, recording why. This is the worst shape of node failure there
  is: it registers on time, so every liveness check passes and `pick_node` keeps choosing it, and
  each job it is handed loses a minute before going somewhere else. `/lsnode` and the worker
  snapshot's `drain_reason` name it; `/drainnode name:<node> drain:false` puts it back and clears
  the count.
- **`max_jobs` is clamped, not believed.** It is the node's own answer about itself, arriving off
  the wire; one mistyped digit would have the coordinator hand it the whole queue and then wait for
  all of it.
- **Abandon.** A node that renews a lease the coordinator has already reclaimed is told
  `abandon: true` and drops the work. This is the only thing that stops two machines finishing the
  same job.
- **Requeue budget.** `LINK_MAX_ATTEMPTS` (2). Past it a job stays local, where the encode stall
  watchdog can end it properly, instead of touring the cluster forever.
- **No job ever waits for a node** — except on an [orchestrator](#orchestrator-mode), which is the
  whole of what that mode changes. `choose_node` returning an error means the job runs here, so a
  cluster that is full, drained, absent or reserved elsewhere never holds work up. `link_only_node`
  is the one exception in effect: it stops every other node being offered anything, and those jobs
  fall back to the coordinator rather than queueing for the named one.
- **Declined.** A node that cannot run a job at all — a preset it does not have — reports `declined`
  and the job runs locally without spending a retry, since nothing was attempted. The node is also
  remembered against that job. On a coordinator that encodes nothing ever reads that, because the
  job has already gone local; on an [orchestrator](#orchestrator-mode) it is what stops the job
  being offered straight back to the node that just refused it.
- **Cancel.** Cancelling a leased job sets a flag on the node's next renew; the node then takes its
  own ordinary local cancel path and reports back, so the job ends through the same events as any
  other remote transition. The cancellation is also recorded on the coordinator's own copy of the
  job, and that is what a node which never answers runs into: the lease expires and looks exactly
  like one that was lost, so without it the watchdog requeues the job and runs to completion the
  very work somebody asked to stop. A cancelled job whose lease expires ends as cancelled.

  **A batch cancels its leased episodes the same way.** Cancelling a batch parent writes a `CANCEL`
  marker into each child's work directory, which is the right thing for a child encoding here and
  meaningless for one that is not: a leased child's directory on this machine is empty, and the
  file lands where nothing ever reads it. Leased children take the renew path instead.
- **A leased job is never a duplicate source.** Its input was downloaded on the node, so this
  machine's copy of its work directory is empty; advertising it would hand another job a path with
  no video behind it. A job merely waiting for a node has downloaded nothing anywhere and is
  excluded for the same reason.

### Failures that used to be silent

Three things in this path could stop the cluster without saying anything, and none of them would
have named itself in a log:

- **A poisoned mutex.** The link board, a node's pending-report buffer and the asset hash cache are
  all plain maps behind a `Mutex`, and one panic while a lock was held turned every later
  acquisition into a panic of its own — naming the lock rather than the fault. On the coordinator
  that is `pn_worker` dying and the whole queue stopping; on a node it is a machine that encodes
  and reports nothing. Poisoning is recovered from (`lib::sync::lock`) rather than propagated: the
  first panic still reaches the log where it happened, and nothing after it is turned into a
  second, less informative failure.
- **The worker loop ending.** `pn_worker` is a spawned task and nothing noticed if it stopped —
  Discord kept answering, the API kept accepting submissions, and each one landed in a channel
  nobody read again. It is watched now, and its ending exits the process so the restart loop can
  bring the queue back.
- **Blocking work on the async runtime.** Building the asset manifest walks and hashes the whole
  font and intro corpus; a node's log chunks are appended file by file. Both ran inline — the first
  inside `pn_worker`'s loop and inside the board's own lock, the second on the thread serving the
  API — so a large corpus or a busy cluster showed up as leases timing out and a coordinator that
  paused, neither of which anything reported. The manifest is now kept warm off the runtime and
  every remaining walk and append runs on a blocking thread.

An unreadable `link_nodes.json` is also set aside as `link_nodes.json.unreadable` rather than
overwritten by the first save that follows it, so a roster the coordinator could not parse does not
silently take every drain flag, `/teenode` group and `/limit` reservation with it.

## Observability

`/lsnode` (rank 4) lists the roster: every node, what it is for, its thread count, how many jobs it
may hold, the build it is level with, how long ago it was heard from, and what it is running — plus
the coordinator's own build to compare against, a warning line under any node whose last
migration failed, and the reason under any node the coordinator drained itself — and warns when
`link_enabled` is off, when `link_only_node` is pinning offload, and when this coordinator is an
[orchestrator](#orchestrator-mode), where an empty roster stops everything rather than merely
costing throughput. `/drainnode name:<node> [drain:<bool>]` stops offering work to
a node (it finishes what it holds) or puts it back in rotation; the flag is persisted, since
draining before a deploy does not mean until the next one. `/rmnode name:<node>` forgets a node —
it re-registers on its next poll unless its token is revoked with `/rmtoken`, and any job it still
holds is reclaimed when its lease expires. `/teenode name:<node> [group:<name>]` groups a node
under a shared worker name (see [Grouping](#grouping)); `/lsnode` shows it as `→ lnk-<group>` beside
the node's own name. `/limit name:<node> [clear:<bool>]` reserves a node for the server the command
was used in (see [Reserving a node for one server](#reserving-a-node-for-one-server)).

`GET /api/v1/workers` (privileged only) gains a `nodes` array — name, `/teenode` group, `/limit` reservation (`reserved_for`, a guild id as a string or null), purpose,
thread count, `max_jobs`, measured encoders, drain state and `drain_reason`, seconds since last
contact, the jobs it holds, its `encoder_identity`, the `build` it is level with, and any
`migration_error`.
Queue entries gain `link_node`, `link_attempts`, `link_waiting` and `link_wait_reason`.
See [API.md](API.md#worker-snapshot).

Link activity prints as `[link] <node> | <message>` on the coordinator and `[link] <message>` on a
node, matching the `[lumiere]` convention.

## Not yet implemented

- **Log proxying.** A node's tool logs stay on the node; `/catlogs` and `GET /jobs/:id/logs` answer
  only from the coordinator's own copy. Nodes are to push a bounded tail on renew and a bundle with
  the result.
- **Batch split at ≈1× download**, on a coordinator that also encodes. Episodes are leased one
  index at a time (see [Batch split](#batch-split)), but the parent still downloads the whole
  selection, so a leased episode is fetched twice. Holding cluster-wide download to ≈1× needs the
  parent's own selection narrowed, and `FileSelection` is turned into a piece bitmap when the
  session opens — narrowing it means either deciding every lease before the parent's download
  starts, which can only use the nodes that happen to be free at that instant, or a control channel
  into a running `pnp2p`. [Orchestrator mode](#orchestrator-mode) sidesteps it rather than solving
  it: there the parent downloads nothing, so there is nothing to narrow.
