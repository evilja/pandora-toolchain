# Cloudflare Lumiere Worker

Dependency-free Cloudflare Worker control plane for Pandora uploads. It stores reusable credentials in Worker secret bindings and never accepts file bodies.

Deployment, profile format, Tunnel routing, migration, and smoke-test instructions are in [`../../docs/LUMIERE_BROKER.md`](../../docs/LUMIERE_BROKER.md).

Do not commit `wrangler.toml`, `.dev.vars`, provider keys, broker tokens, or populated Drive profile JSON.
