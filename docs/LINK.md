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
client. No `discord_token` is needed. Every job a node runs uses `Frontend::None`, which the worker
pipeline already treats as a no-op for every message edit, reaction and presence update.

A node keeps its own sqlite database because `pn_worker` needs one. It is scratch, not a record:
`fail_stale_active()` at startup means a restarted node never resumes a job, matching the
deliberately non-resuming reboot policy in [WORKER.md](WORKER.md).

## Configuration

Node side:

| Key | Meaning |
| --- | --- |
| `pandora_mode` | `mini` enables link mode (equivalently `--mini`) |
| `link_coordinator_url` | coordinator origin, no trailing slash |
| `link_node_token` | this node's `\|link\|` token |
| `link_node_name` | stable node identity; must match the token's node name |
| `link_max_jobs` | concurrent leases, default `1` |

Coordinator side:

| Key | Meaning |
| --- | --- |
| `link_enabled` | master switch for offload; without it nothing is ever leased |
| `link_only_node` | limit all offload to one named node |
| `link_lease_timeout_secs` | how long a silent lease is kept, default `90` |
| `link_allow_build_mismatch` | permit a node whose pnmpeg differs from the coordinator's |

State:

- `DB/config/global/environment/link_nodes.json` (coordinator, mode `0600`) — the node roster as a
  JSON array of `{ name, pandora_version, pnmpeg_build, encoder_digest, ffmpeg_version, threads,
  max_jobs, presets, registered_at, last_seen, drain }`. The roster is advisory: a node
  re-registers within seconds of coming up. It is persisted for the one field a restart must not
  forget — an operator's drain flag.

## Tokens

Node tokens live in the same `api.pandora` as every other token, as `<token>|link|<node>`. Mint one
with `/gentoken link:<node>` (see [DISCORD.md](DISCORD.md)); `local` and `link` are mutually
exclusive, and a node name may contain neither whitespace nor `|`.

A link token opens **only** the `/api/v1/link/*` routes and nothing else — it cannot submit jobs,
read logs, or reach git. Every link route additionally checks that the node named in the request
body matches the node the token is bound to, so one node cannot renew or finish another's lease.
Link traffic is exempt from the API write rate limit: a node renews every lease every ten seconds
for as long as it works, which is a heartbeat on a fixed cadence rather than user traffic.

## Routes

All under `/api/v1/link/`, all requiring a link token.

- `POST /link/register` — the node announces itself (name, versions, `encoder_digest`, thread count,
  `max_jobs`). Returns `{ accepted, reason?, renew_secs, lease_timeout_secs }`.
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
- `GET /link/assets/manifest` — the font and intro corpus, with its revision. See [Assets](#assets).
- `GET /link/assets/:hash` — one asset, addressed by content hash.

## Build parity

`register` carries an `encoder_digest`: the SHA-256 of the node's own `pnmpeg` binary. pnmpeg links
x264 statically, so equal digests mean the same encoder — a stronger guarantee than comparing
version strings, and one that costs a single hash at startup.

The coordinator **refuses a mismatch by default**. An episode is encoded entirely on one machine, so
a mismatch cannot corrupt a file; but two builds make different rate decisions at the same CRF, and
a cluster that quietly ships two quality tiers is not worth debugging later. A deliberately
heterogeneous cluster — mixed architectures, say — has genuinely different builds and says so with
`link_allow_build_mismatch` rather than having the check weakened for everybody.

## Assets

Fonts and intro videos are the two things a node needs that do not travel in a job spec, and a
missing font **does not fail** — libass substitutes one and the release goes out in the wrong
typeface with nothing to show for it. The link closes that by syncing the coordinator's whole asset
corpus and refusing any job a node cannot prove it holds the corpus for.

The corpus is compared **by content**, never by name or timestamp: two machines agree when their
files hash the same, which is the only definition that survives a copy, a re-download, or a
filesystem that rounds mtimes.

- **The manifest** (`GET /link/assets/manifest`) lists every file under `DB/fontconfig/<bucket>/`
  and every file in every folder an `intros.toml` group resolves to, as
  `{ hash, kind, group, name, bytes }`. Its `revision` is a SHA-256 over the sorted entries, so it
  changes exactly when the corpus does and needs no counter to bump and no hook in `/cfont` to
  remember to call. Files are hashed once per `(path, mtime, len)` and the assembled manifest is
  held for a minute, so a node polling every ten seconds costs one scan.
- **Fetching** (`GET /link/assets/:hash`) serves strictly by content hash, and only for a hash the
  current manifest lists. A node cannot ask for a path, which is what keeps this from being an
  arbitrary read of the coordinator's disk. Entry names that contain a separator or begin with a
  dot are refused on both sides.
- **Reconciling** happens on registration — before the first lease poll, so the common case is a
  node that is already current when it is offered work — and again between jobs whenever a renew
  reports a revision the node has not reached. Only missing entries are fetched, and each is
  verified against its hash before it is written beside its target and renamed into place, so a
  half-downloaded font is never a font libass can find.
- **Installing.** libass resolves through **system fontconfig**, not `DB/fontconfig` — the `ass=`
  filter passes no `fontsdir` — so a synced font is not yet a usable font. After a reconcile that
  added any font, the node re-runs the same startup installer that copies `DB/fontconfig/<bucket>`
  into the OS font path and runs `fc-cache`, then re-warms the font-name index.
- **Refusing.** A leased job whose `assets_revision` the node has not reached triggers one inline
  reconcile; if it still does not match, the node **declines the lease** and the coordinator runs
  the job locally. Declining costs no retry, since nothing was attempted.

Synced files land in `DB/fontconfig/<bucket>/` (fonts, the same place the startup installer reads)
and `DB/cache/link-intros/<group>/` (intros, deliberately apart from anything an operator
hand-placed, since pnmpeg writes compatibility variants back into whatever intro folder it is
given). The last fully-synced revision is recorded in `DB/cache/link-assets-revision`.

### Intro groups

A job snapshots the **folder** its server's intro group resolved to, inside its `Preset` variant's
`Option<String>` — and that path means nothing on another machine. What travels is the group's
*name*, recovered by reverse lookup against `intros.toml` so it reflects the job's own snapshot
rather than settings that may have changed since. The node rebuilds the preset with its own synced
folder for that group, and declines the lease if the group materialised no files — an empty intro
folder would otherwise produce a release with no intro, which is the same class of silent failure as
a substituted font.

## What can be leased

The rule is "does the job carry its own source", since a node fetches its own input:

- **Leasable**: `Encode`, `Pancode`, `Backup`, `Probe`, provided `job.torrent` is a non-empty link.
- **Never leased**: forwarded jobs (they mirror another job's outcome and run nowhere); batch
  parents and children (a parent is one torrent download feeding many children, and a child is
  hard-linked out of it, so neither carries a source of its own); keeps, `Keycode`, `Preview` and
  `Studio` (their inputs are files on the coordinator); and any job past `LINK_MAX_ATTEMPTS`.

A leased Pancode carries its originating `probe_job_id` alongside the source link. The node has no
probe job and will fail to adopt its saved `.torrent`; the link is what the download falls back to,
and the file index selects the episode.

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
undrained, under its `max_jobs`, and advertises the job's preset; otherwise the job runs locally
exactly as before. **A job never waits for a node** — the cluster being full, drained or absent is
never a reason for work to sit still.

Nodes are ranked most-idle first, then by thread count, so a cluster of unequal machines fills its
biggest free box before its smallest. `link_only_node` limits offload to a single named node, which
is how a node is trialled or a misbehaving one bypassed without deleting its token.

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

## Failure handling

A remote job is cheap to lose: its inputs are a link and a few KB of subtitle, so **requeue, not
recovery**, is the response to every node failure.

- **Lease expiry.** No renew for `link_lease_timeout_secs` (default 90) and the coordinator reclaims
  the job, returning it to the queue as an ordinary local candidate. An offered lease nobody
  collects is given a shorter rope — 60 seconds — because nothing has started.
- **Abandon.** A node that renews a lease the coordinator has already reclaimed is told
  `abandon: true` and drops the work. This is the only thing that stops two machines finishing the
  same job.
- **Requeue budget.** `LINK_MAX_ATTEMPTS` (2). Past it a job stays local, where the encode stall
  watchdog can end it properly, instead of touring the cluster forever.
- **Declined.** A node that cannot run a job at all — a preset it does not have — reports `declined`
  and the job runs locally without spending a retry, since nothing was attempted.
- **Cancel.** Cancelling a leased job sets a flag on the node's next renew; the node then takes its
  own ordinary local cancel path and reports back, so the job ends through the same events as any
  other remote transition. A cancel that cannot be delivered is satisfied by the lease expiring.
- **A leased job is never a duplicate source.** Its input was downloaded on the node, so this
  machine's copy of its work directory is empty; advertising it would hand another job a path with
  no video behind it.

## Observability

`/lsnode` (rank 4) lists the roster: every node, its thread count, how many jobs it may hold, how
long ago it was heard from, and what it is running — and warns when `link_enabled` is off or
`link_only_node` is pinning offload. `/drainnode name:<node> [drain:<bool>]` stops offering work to
a node (it finishes what it holds) or puts it back in rotation; the flag is persisted, since
draining before a deploy does not mean until the next one. `/rmnode name:<node>` forgets a node —
it re-registers on its next poll unless its token is revoked with `/rmtoken`, and any job it still
holds is reclaimed when its lease expires.

`GET /api/v1/workers` (PNwitch token only) gains a `nodes` array — name, thread count, `max_jobs`,
presets, drain state, seconds since last contact, the jobs it holds, and its `encoder_digest`.
Queue entries gain `link_node` and `link_attempts`. See [API.md](API.md#worker-snapshot).

Link activity prints as `[link] <node> | <message>` on the coordinator and `[link] <message>` on a
node, matching the `[lumiere]` convention.

## Not yet implemented

- **Log proxying.** A node's tool logs stay on the node; `/catlogs` and `GET /jobs/:id/logs` answer
  only from the coordinator's own copy. Nodes are to push a bounded tail on renew and a bundle with
  the result.
- **Batch split.** Batch children carry no source of their own. Assigning each an index of the same
  torrent via `pnp2p --selects` keeps cluster-wide download at ≈1× the torrent, but needs the
  parent's own selection narrowed and `BatchRequest::settle_download` taught that an episode can
  complete without a local `TORRENT_FILE_DONE`.
