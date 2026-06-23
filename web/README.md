# Pandora web console

A single self-contained page (`index.html`, no build step, no dependencies) that drives the
pndc HTTP API: submit encode/backup jobs, list/inspect jobs, and cancel them. Auth is the
same bearer token as the API (`DB/config/global/environment/api.pandora`), entered in the
footer and stored in the browser's `localStorage`.

## How it talks to the API

The page calls **relative** paths (`/api/v1/...`), so it must be served from the **same
origin** as the API. The intended setup: one nginx vhost on `api.<domain>.com` that serves
these static files at `/` and reverse-proxies the API paths to the bot's loopback port.

```nginx
server {
    server_name api.<domain>.com;
    root /var/www/pandora-ui;        # deploy web/index.html here
    index index.html;

    location / { try_files $uri $uri/ /index.html; }

    location /api/ {                 # proxies /api/v1/... unchanged
        proxy_pass http://127.0.0.1:8787;
        proxy_set_header Host $host;
        proxy_set_header Authorization $http_authorization;
    }
    location = /health { proxy_pass http://127.0.0.1:8787; }

    # TLS: certbot / your existing cert setup
}
```

Replace `8787` with the bot's `api_port` (from `env.pandora`).

## Deploy

```sh
cp web/index.html /var/www/pandora-ui/index.html
```

That's it — no other assets.

## Local testing (same-origin without nginx)

Run `pndc` with `api_port` set and a token in `api.pandora`, then put the UI and API behind a
tiny proxy so they share an origin. With Caddy:

```
:8080 {
  handle /api/* { reverse_proxy 127.0.0.1:8787 }
  handle /health { reverse_proxy 127.0.0.1:8787 }
  handle { root * ./web; file_server }
}
```

Visit `http://localhost:8080`, paste a token, and run a command.

## Notes

- `job_id` is a numeric **string** (it exceeds JS's safe-integer range); copy it as-is from a
  submit response into **Get Job** / **Cancel**.
- **Get Job** has an optional "auto-refresh every 2s" toggle that polls until the job reaches a
  terminal stage (Uploaded / Failed / Declined / Cancelled).
- Encode reads the `.ass` file in the browser and base64-encodes it before sending.
