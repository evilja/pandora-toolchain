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
