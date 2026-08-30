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
  JSON array of `{ name, pandora_version, encoder_identity, ffmpeg_version, threads,
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

- `POST /link/register` — the node announces itself (name, version, `encoder_identity`, thread
  count, `max_jobs`). Returns `{ accepted, reason?, renew_secs, lease_timeout_secs }`.
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

A probe is the other outcome that does not end its job. `Probed` is where a probe stops locally
too — it then waits for a file to be selected, and the probe timeout archives it — so the node
reports `probed`, the coordinator releases the lease and clears `link_node`, and the job stays in
the queue exactly where a local probe would leave it. Without that the lease would simply expire
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
presets, drain state, seconds since last contact, the jobs it holds, and its `encoder_identity`.
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
