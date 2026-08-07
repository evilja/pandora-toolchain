# Lumiere credential broker

Lumiere removes reusable upload-provider credentials from the Pandora VDS. The Cloudflare Worker performs only authenticated control-plane calls:

- Google Drive: the Worker creates a resumable session; Pandora sends all file bytes directly from the VDS to Google.
- DoodStream, LuluStream, and Voe: the Worker starts a remote upload; each provider downloads the bytes from a temporary capability URL served by `pndc` on the VDS.
- Abyss: disabled/fail-closed until its authenticated remote-upload API is documented.

The Worker never receives a video body. If `lumiere_public_url` is a Cloudflare Tunnel hostname, file bytes traverse Cloudflare Tunnel/CDN transport but do not execute in the Worker.

## 1. Choose two public URLs

Use separate hostnames when possible:

- `https://lumiere-api.example.com` — the Cloudflare Worker.
- `https://pandora-files.example.com` — the existing `pndc` Axum server through Cloudflare Tunnel.

In **Zero Trust > Networks > Tunnels > your tunnel > Public Hostnames**, add `pandora-files.example.com` with service `http://pndc:8787`. The Compose `cloudflared` container and `pndc` already share the `pandora` network.

The file hostname must not require Cloudflare Access login because DoodStream/LuluStream/Voe cannot provide Access credentials. Lumiere URLs contain a random 256-bit capability, expire, are held only in memory, and are removed when the upload task ends. Configure Cloudflare to bypass cache and browser/Bot challenges for `/lumiere/v1/files/*`; provider fetchers must receive the file rather than an HTML challenge.

If Cloudflare must not carry file bytes at all, expose an HTTPS origin on the VDS directly and use that origin as `lumiere_public_url` instead of a Tunnel hostname.

## 2. Prepare the Worker

Run these commands on a trusted administration workstation, not on the VDS. Protect the Cloudflare account and deploy tokens with MFA and least privilege: anyone who can deploy modified Worker code can make that code use or exfiltrate its secret bindings.

```sh
cd cloudflare/lumiere-broker
cp wrangler.toml.example wrangler.toml
npm install
npx wrangler login
npx wrangler kv namespace create OPERATIONS
```

Put the returned namespace ID into `wrangler.toml`, and set:

```toml
[vars]
LUMIERE_SOURCE_ORIGIN = "https://pandora-files.example.com"
```

`LUMIERE_SOURCE_ORIGIN` must be only the origin: no trailing slash or path. The Worker rejects every remote-upload URL outside this origin and `/lumiere/v1/files/`.

## 3. Create the scoped Pandora-to-Worker token

Generate at least 32 random bytes. For example in PowerShell:

```powershell
$bytes = New-Object byte[] 32
[Security.Cryptography.RandomNumberGenerator]::Fill($bytes)
$LumiereToken = [Convert]::ToBase64String($bytes)
$LumiereToken
```

Store it as a Worker secret interactively:

```sh
npx wrangler secret put LUMIERE_CLIENT_TOKEN
```

This token is stored on the VDS, so a VDS compromise can invoke the Worker's narrow upload operations. It cannot retrieve provider credentials or call arbitrary provider URLs.

## 4. Configure Drive profiles

Copy `drive-profiles.example.json` to a temporary file outside the repository. Profiles are selected deterministically:

- `global` is the mandatory fallback profile.
- `guild:<Discord server id>` is preferred when `/edit local_gdrive:true` and that profile exists.

Root names have fixed meanings:

- Global profile: `default`.
- Guild profile: `smartcode` and/or `anonymous`. If one is absent, Pandora tries the other before global fallback.

Example:

```json
{
  "global": {
    "client_id": "...",
    "client_secret": "...",
    "refresh_token": "...",
    "token_url": "https://oauth2.googleapis.com/token",
    "roots": { "default": "GLOBAL_PARENT_FOLDER_ID" }
  },
  "guild:123456789012345678": {
    "client_id": "...",
    "client_secret": "...",
    "refresh_token": "...",
    "token_url": "https://oauth2.googleapis.com/token",
    "roots": {
      "smartcode": "SMARTCODE_PARENT_FOLDER_ID",
      "anonymous": "ANONYMOUS_PARENT_FOLDER_ID"
    }
  }
}
```

Install the complete JSON object as one encrypted Worker secret:

```sh
npx wrangler secret put LUMIERE_DRIVE_PROFILES
```

Paste compact JSON when prompted. Cloudflare Worker variables/secrets are limited to 5 KB each. Check the compact profile map before deployment. If the deployment grows beyond that limit, use Secrets Store, application-layer-encrypted D1 with a separately held data key, or split profiles across Workers; do not put plaintext credentials in KV.

## 5. Configure streaming-host keys

Set only the providers in use:

```sh
npx wrangler secret put DOODSTREAM_API_KEY
npx wrangler secret put LULUSTREAM_API_KEY
npx wrangler secret put VOE_API_KEY
```

There is intentionally no Abyss secret binding yet.

## 6. Deploy and route the Worker

```sh
npm run check
npx wrangler deploy
```

Either use the generated `workers.dev` URL or add `lumiere-api.example.com` as the Worker's custom domain in **Workers & Pages > lumiere-broker > Settings > Domains & Routes**. A custom domain is recommended: if the VDS has stable outbound IP, add a WAF rule allowing `/v1/*` only from that IP and add a conservative rate-limit rule. Do not apply that IP restriction to `pandora-files.example.com`, because provider fetch IPs are not stable.

A public health check reveals no configuration:

```sh
curl https://lumiere-api.example.com/health
```

Expected response:

```json
{"ok":true,"version":"1"}
```

All `/v1/*` routes require both the bearer token and `X-Lumiere-Version: 1`.

## 7. Configure Pandora

Add these entries to `DB/config/global/environment/env.pandora`:

```text
lumiere_broker_url|pntools|https://lumiere-api.example.com
lumiere_broker_token|pntools|PASTE_THE_RANDOM_CLIENT_TOKEN
lumiere_public_url|pntools|https://pandora-files.example.com
lumiere_transfer_ttl_secs|pntools|21600
lumiere_poll_interval_secs|pntools|5
```

`api_port` must remain enabled and the Tunnel hostname must point to the same `pndc` service. The transfer TTL must cover provider queueing plus download time; accepted values are 5 minutes through 24 hours, capabilities remain memory-only, and restarting `pndc` invalidates active transfers. Rebuild/restart Pandora after deploying the code:

```sh
docker compose up -d --build
```

Use `/providers` to verify that the Worker reports the global/guild Drive profile and each configured streaming host.

## 8. Smoke test before deleting local credentials

Use a small, non-sensitive file and a provider account where creating a test file is acceptable. A test upload is mutating: it creates Drive/provider records and may consume remote-upload queue slots.

Verify:

1. Google Drive progress is produced by the VDS and the final file size and MD5 match.
2. DoodStream/LuluStream/Voe fetch `/lumiere/v1/files/...` successfully, including `HEAD`, `GET`, retries, and byte ranges.
3. Capability URLs return `404` after the upload task finishes.
4. Worker logs contain no bearer token, provider key, Google session URI, or capability URL.
5. Smartcode replacement deletes the previous Drive file through its per-file Lumiere deletion capability. The first replacement of a pre-Lumiere state intentionally requires manual cleanup because legacy state has no such capability.

## 9. Remove legacy secrets from the VDS

Only after a successful smoke test and an offline recovery backup:

- Remove or blank global `gdrive_client_id`, `gdrive_client_secret`, `gdrive_refresh_token`, `gdrive_token_url`, and `gdrive_parent_id`.
- Remove or blank `doodstream`, `lulu`, and `voesx`.
- Preserve the Abyss key offline until a supported broker API exists, then remove it from the VDS. Pandora currently marks Abyss unavailable.
- In each `DB/config/<guild>/meta.pandora`, blank legacy lines 4-7 and 10 while preserving line positions. Lines 4-6 were OAuth credentials; lines 7/10 were roots now stored in the Worker profile.

Do not delete the only recovery copy until the new uploads and Smartcode cleanup have been exercised. Stop Pandora before changing these files. From PowerShell in the repository root, the following preserves the positional guild metadata format while blanking only migrated fields:

```powershell
docker compose stop
$utf8 = [System.Text.UTF8Encoding]::new($false)
$sep = "|pntools|"
$migrated = @{
  gdrive_client_id = $true; gdrive_client_secret = $true
  gdrive_refresh_token = $true; gdrive_token_url = $true
  gdrive_upload_url = $true; gdrive_parent_id = $true
  doodstream = $true; lulu = $true; voesx = $true; abyss = $true
}
$global = (Resolve-Path "DB/config/global/environment/env.pandora").Path
$lines = @(Get-Content -LiteralPath $global | ForEach-Object {
  $line = $_
  $at = $line.IndexOf($sep)
  if ($at -ge 0 -and $migrated.ContainsKey($line.Substring(0, $at))) {
    $line.Substring(0, $at) + $sep
  } else { $line }
})
[System.IO.File]::WriteAllLines($global, $lines, $utf8)

Get-ChildItem "DB/config" -Directory | ForEach-Object {
  $meta = Join-Path $_.FullName "meta.pandora"
  if (Test-Path -LiteralPath $meta) {
    $lines = @(Get-Content -LiteralPath $meta)
    while ($lines.Count -lt 11) { $lines += "" }
    foreach ($index in @(4, 5, 6, 7, 10)) { $lines[$index] = "" }
    [System.IO.File]::WriteAllLines($meta, $lines, $utf8)
  }
}

@("DB", "work") | Where-Object { Test-Path $_ } |
  ForEach-Object { Get-ChildItem $_ -Recurse -File -Filter "*gdrive_env.pandora" } |
  Remove-Item -Force
docker compose up -d
```

After verifying the scrub, rotate every legacy Google and streaming-provider credential because it previously existed on the VDS: install each replacement in the Worker, retest, and only then revoke the old value. Keep `lumiere_broker_token`: Pandora needs that narrow broker credential, and it is not a provider key.

## API and security properties

- The Worker accepts only typed Drive/session/delete and three remote-upload operations.
- Provider endpoints and source origin are hard-coded/allowlisted.
- KV stores idempotency records containing temporary source URLs and provider operation IDs for 24 hours; it never stores provider credentials.
- Drive session URLs are pinned to `https://www.googleapis.com/upload/drive/v3/files` by both Worker and Rust client.
- Every Drive session uses a pre-generated file ID and receives a random deletion capability whose hash is bound into private Drive `appProperties`; the Worker verifies it before deleting, so the VDS broker token alone cannot delete arbitrary account files. Smartcode state stores this narrow capability with mode `0600` on Unix.
- Pandora verifies Drive size and MD5, requesting capability-checked deletion of an unverified result through the broker.
- File-transfer capabilities are 256-bit random tokens, memory-only, range-capable, non-cacheable, and scoped to one exact file.
- A VDS attacker can abuse operations Pandora is allowed to invoke while its broker token remains valid. Preventing that requires an independently authorized control plane or remote attestation; the broker's purpose here is preventing reusable credential extraction.

## Cloudflare references

- [Workers secrets](https://developers.cloudflare.com/workers/configuration/secrets/)
- [Workers limits](https://developers.cloudflare.com/workers/platform/limits/)
- [Workers KV setup](https://developers.cloudflare.com/kv/get-started/)
- [Worker custom domains](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/)
- [Cloudflare Tunnel routing](https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/routing-to-tunnel/)
