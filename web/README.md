# Pandora web console

A single self-contained page (`index.html`, no build step, no dependencies) that drives the
pndc HTTP API: submit encode/backup jobs, list/inspect jobs, and cancel them. Auth is the
same bearer token as the API (mint one with `/gentoken`, stored in
`DB/config/global/environment/api.pandora`), entered in the footer and saved in the browser.

## The bot serves this page itself

`index.html` is **baked into the `pndc` binary** (`include_str!`) and served by the API server.
When `api_port` is set in `env.pandora`, the bot listens on that port and answers:

- `GET /`            → this console
- `GET /api/v1/...`  → the JSON API (same origin, so no CORS)
- `GET /health`      → liveness

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

- `job_id` is a numeric **string** (it exceeds JS's safe-integer range); the Get Job / Cancel
  dropdowns pull live ids from the API so you don't have to copy them by hand.
- **Get Job** has an optional "auto-refresh every 2s" toggle that polls until the job reaches a
  terminal stage (Uploaded / Failed / Declined / Cancelled).
- Encode reads the `.ass` file in the browser and base64-encodes it before sending.
