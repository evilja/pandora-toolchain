# Pandora web console

One application shell over the `pndc` HTTP API — no build step, no dependencies. Every page links
two shared assets and supplies only its own content:

- **`shell.css`** (`GET /console.css`) — the design system: `--pn-*` tokens, and every shared
  component (rail, topbar, cards, tables, forms, buttons, progress, stepper, chips, toast).
- **`shell.js`** (`GET /console.js`) — draws the rail and topbar from one `NAV` table and exposes
  `PN.*`: `shell()`, `api()`, `bar()`, `stepper()`, `chip()`, `icon()`, `toast()`, `navCount()`,
  `getToken()`/`setToken()`/`clearToken()`, `setTheme()`, `refreshIdentity()`.

**Change shared chrome in those two files, not in four HTML documents.** A page ships
`.pn-app > .pn-main > .pn-content` and calls `PN.shell({ page, title, actions })` once.

The pages:

- **`index.html`** — the console, serving four views off `location.pathname`:
  - `/` **Operations** — stat tiles, the live pipeline, worker capacity, recently finished.
  - `/jobs` **Jobs** — filter tabs, search, and a detail panel that polls the selected job and
    carries **Cancel job**.
  - `/encode` **Encode** — tabs for Encode / Git Encode / Backup / Pancode / Keycode, with a
    submission summary and preset guidance beside them.
  - `/settings` **Settings** — the bearer token, its probed reach, theme, and job polling.
- **`git.html`** (`/git`) — **Repositories**: the attached-anime table, a details card with Source
  and Smartcode tabs plus Detach/Destruct, New repository / Attach existing forms, and the README
  template editor. Needs a **local** token (see below).
- **`studio.html`** (`/studio`) — the Studio Cutroom: media pool, program monitor, inspector,
  draggable audio clips, multitrack timeline. It keeps its dense editor chrome but takes every
  colour from the shell tokens. `studio-sw.js` bridges authenticated byte-range video requests;
  Web Audio performs preview mixing in the browser.
- **`batch.html`** (`/batch/<token>`) — the read-only output page for an `/encode batch` job: every
  episode's stage, pairing, and upload links as near-plaintext, fetched with `no-store` so
  Cloudflare cannot serve a stale view of a running batch. It is deliberately not part of the shell
  — it is a public capability link, not a console.
- **`../kagami-trace/web/index.html`** (`/trace`) — the Kagami Trace Lab. It injects the shell only
  when served at `/trace`, so standalone `pntrace` requests neither asset and the sub-crate stays
  extraction-ready.

## Design

The palette is taken from the reference designs: a flat near-black ground (`#05101d` content,
`#091524` rail, `#0e1927` cards), one blue accent (`#2562c3` buttons, `#3c81eb` links, progress and
icons), and green / amber / red reserved for status. Small radii (6px; 8px for cards), no panel
shadows, and the only serif anywhere is the `PANDORA` wordmark — page titles and stat values are
the body sans, bold.

Progress is drawn two ways, both from a job's `stage` + `progress`: `PN.bar()` for a table row
(one blue track; green only once the job *finished*, red when it failed) and `PN.stepper()` for the
job detail (Queue / Download / Encode / Upload, each Completed / In progress / Pending). Under the
stepper the console prints the line an encoder actually watches —
`41% · frame 18422/34071 · 41.2 fps · 4210 kbit/s · ETA 13m`.

Auth is the same bearer token as the API (mint one with `/gentoken`, stored in
`DB/config/global/environment/api.pandora`). It is entered once in **Settings**, saved to
`localStorage` under `pandora_token`, and shared by every console — including the Trace Lab and
Studio. Settings also probes what the token can reach and offers **Forget saved token** (this
browser) next to **Revoke on the server** (`POST /api/v1/token/revoke`).

Theme is `pandora_theme` in `localStorage`: `dark`, `light`, or `system` (the default). It is
chosen in Settings, follows `prefers-color-scheme` while set to system, and repaints other open
tabs through the `storage` event. Retheme by editing the `:root` blocks in `shell.css` — that is
the only place colours are defined.

Below 1080px the rail becomes a horizontal icon bar and the two-column layouts stack; below 700px
the topbar's page action is dropped in favour of the rail. Keyboard focus is visible throughout and
`prefers-reduced-motion` collapses every transition.

## The bot serves these pages itself

All of them are **baked into the `pndc` binary** (`include_str!`) and served by the API server.
When `api_port` is set in `env.pandora`, the bot listens on that port and answers:

- `GET /`, `/jobs`, `/encode`, `/settings` → the console (`index.html`)
- `GET /git`         → Repositories (`git.html`)
- `GET /studio`      → the Studio Cutroom (`studio.html`)
- `GET /trace`       → the Kagami Trace Lab (`../kagami-trace/web/index.html`)
- `GET /console.css` → the shared design system (`shell.css`)
- `GET /console.js`  → the shared shell (`shell.js`)
- `GET /batch/<token>` → a batch encode's output page (`batch.html`), authorized by the token in the URL rather than a bearer token, since the link is posted to Discord
- `GET /studio-sw.js` → Studio authenticated-stream service worker
- `GET/POST /api/v1/...` → the bearer-protected API (same origin, so no CORS)
- `GET /health`      → liveness

### Local tokens & the git console

A token line in `api.pandora` may carry a `|local|<server_id>` suffix; mint one with
`/gentoken local` (run it in the target Discord server). A local token is **bound to that
server**: it uses the server's Google Drive credentials for uploads, and it is **required** for
the git endpoints (`GET /api/v1/git/{attachments,channels}`,
`POST /api/v1/git/{init,attach,source}`) — a plain token gets `403` there. The server id comes
from the token; the channel id is per request (sent as a string because Discord snowflakes exceed
JS's safe-integer range).

The Studio editor also requires a local token. Source video is streamed from the current Studio with HTTP byte ranges; audio assets are decoded and mixed locally for insert, override, and duck previews. Audio clips can be dragged along the timeline with frame snapping and a live frame/timecode readout; the inspector also accepts an exact start frame using the Studio source FPS. Adding, moving, trimming, removing, or changing an audio clip updates the browser mix without stopping video playback. Audio uploads are limited to 50 MiB per file, accept any format ffmpeg can decode, show a circular byte-progress notification, and remain visible while the server processes the uploaded media. A clip the browser itself cannot decode (WMA, APE, DTS and similar) stays silent in the local preview and says so — the server render still mixes it. Clip level runs from 0% to 500%. Scrubbing, moving clips, and changing mix controls never queue preview encodes. The **Deliver** action is the only editor action that queues a server render.

Repositories never asks for a raw channel id:

- **Source**/**Smartcode**/**Detach**/**Destruct** act on the row selected in the **attached anime**
  table (`GET /api/v1/git/attachments`, from `DB/config/<server>/*/meta.toml`), which also resolves
  each attachment's channel to its `#name`.
- **New repository**/**Attach existing** pick from a dropdown of the server's **Discord channels**
  (`GET /api/v1/git/channels`, from `DB/config/<server>/channels.json`, which the bot publishes
  and keeps in sync via channel/thread events).

**Smartcode** also takes an episode and an optional source link (blank reads the episode's
`SOURCE.md`); it derives preset/concat from the server's `/edit` settings, merges, uploads the
release, and queues an encode job, linking straight to it in Jobs. Its form does not ask for preset
or concat. **Destruct** deletes the Forgejo repo, so it asks you to type the anime's name back first.

The channel dropdown is refreshable and remembers the last pick in the browser.

When a Discord channel/thread (incl. forum channels and posts) is **deleted**, the bot
auto-detaches it — it removes that channel's `meta.toml` (the repo is left untouched), so deleted
channels stop appearing as attachments.

So there is **nothing else to install or host** — no nginx, no Caddy, no admin rights. Point a
browser at the bot's port and you get the UI. Editing this file requires rebuilding `pndc`.

### Binding

By default the server binds **all interfaces** (`0.0.0.0`), so the machine's public IP reaches
it directly, e.g. `http://<server-ip>:<api_port>/`. To restrict it to loopback (e.g. when you
*do* put a reverse proxy in front), set `api_host` in `env.pandora`:

```
api_host|pntools|127.0.0.1
```

### Reachability

Binding to `0.0.0.0` is necessary but not always sufficient: the port must also be allowed
through any host/cloud firewall (on Hetzner, the Cloud Firewall in the web console; on Windows,
the Defender Firewall). Ports above 1024 don't need admin to *listen*, but firewall rules might.

## Security note

With `0.0.0.0`, the console and `/health` are public. Every **job** operation (list, get,
submit, cancel) still requires a valid bearer token, so the exposed surface is the static UI and
a liveness check. Mint tokens with `/gentoken` (upper-only) and revoke by deleting their line
from `api.pandora`.

## Deploying with Docker + Cloudflare Tunnel

The repo ships a `Dockerfile` and `docker-compose.yml` that run the bot and a `cloudflared`
sidecar on a shared network — no published ports, no inbound firewall holes, TLS handled by
Cloudflare.

1. Set `api_port|pntools|8787` in `env.pandora` (leave `api_host` **unset** so it binds
   `0.0.0.0` and the sidecar can reach it). Mint a token with `/gentoken`.
2. In the Cloudflare Zero Trust dashboard, create a tunnel and set its Public Hostname service to
   **`http://pndc:8787`** — the compose service name, **not** `localhost` (inside the
   `cloudflared` container `localhost` is the container itself, not the bot).
3. Put the tunnel token in a `.env` file beside the compose file: `TUNNEL_TOKEN=...`.
4. `docker compose up -d --build`.

`DB/` is bind-mounted (`./DB:/app/DB`) so the database, `env.pandora`, and `api.pandora` tokens
persist across redeploys. The runtime image bundles `ffmpeg` for the encode pipeline. The image
builds **Linux** containers, so the host must run Docker's Linux engine.

### Native torrent client (torrent/magnet jobs)

Torrent and magnet downloads go through Pandora's in-process BitTorrent v1 client. It supports
HTTP and UDP trackers, TCP peers, BEP 9 magnet metadata, pipelined concurrent piece downloads,
and per-file selection. No separate torrent daemon or host/container save-path mapping is needed;
files are written directly under the bind-mounted `DB/work/...` directory. Google Drive and direct
HTTP video jobs continue to use `pncurl` instead.

The native client intentionally does not implement DHT, uTP, or BitTorrent v2. A magnet therefore
needs at least one working `tr=` HTTP/UDP tracker, and v2-only torrents are rejected.

#### Optional torrent proxy

Set `PNP2P_PROXY` in `.env` to route tracker and peer traffic through one proxy:

```dotenv
# Remote DNS through SOCKS5
PNP2P_PROXY=socks5h://user:password@proxy.example:1080

# Or an HTTP CONNECT proxy
PNP2P_PROXY=http://user:password@proxy.example:8080
```

Supported schemes are `http`, `socks5`, and `socks5h`. HTTP(S) trackers use the configured proxy;
TCP peer connections use SOCKS5 or HTTP CONNECT; SOCKS5 also supports UDP tracker datagrams. An
HTTP proxy cannot carry UDP tracker traffic, so torrents used through one should include an HTTP
or HTTPS tracker. If `PNP2P_PROXY` is unset, `ALL_PROXY` / `all_proxy` is honored before falling
back to direct connections.

Optional tuning variables are `PNP2P_MAX_CONNECTIONS` (default `24`),
`PNP2P_MAX_PEER_CANDIDATES` (default `256`), `PNP2P_PIPELINE` (default `32`),
`PNP2P_BLOCK_SIZE` (default `16384`), `PNP2P_PORT` (default `6881`),
`PNP2P_METADATA_LIMIT` (default `8388608`), `PNP2P_MEMORY_BUFFER` (default
`134217728`), and `PNP2P_TRACKER_ROUNDS` (default `3`).

If instead you run `cloudflared` directly on the host, publish the port (`-p 8787:8787`) and the
dashboard service becomes `http://localhost:8787`.

## Optional: TLS via a reverse proxy

If you later want HTTPS on a domain, set `api_host|pntools|127.0.0.1` and front the bot with a
proxy that terminates TLS and forwards to `127.0.0.1:<api_port>` — e.g. Caddy:

```
api.<domain>.com {
    reverse_proxy 127.0.0.1:8787
}
```

Caddy fetches a Let's Encrypt cert automatically (needs DNS + ports 80/443). This is purely
optional — the bot works standalone without it.

## Notes

- `job_id` is a numeric **string** (it exceeds JS's safe-integer range); you never type one — Jobs
  rows, Operations rows and `?job=<id>` deep links carry it.
- The Jobs detail panel polls the selected job every 2s and stops once it reaches a terminal stage
  (Uploaded / Failed / Declined / Cancelled). Operations and the job tables refresh every 5s.
- Encode reads the `.ass` file in the browser (drag-and-drop works) and base64-encodes it before
  sending. Submitting locks the Encode page to that job until it ends.
- Several things the reference designs show have no data behind them — job timestamps, a "who am
  I" route, worker capacity for non-operator tokens, an events feed, repository health. They were
  left out rather than faked.
