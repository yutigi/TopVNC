# Cloudflare setup helpers

These helpers were moved from `out/multi-host-setup/` so fresh checkouts keep
the reusable scripts. Requires Python 3.11+, Node.js, installed Worker
dependencies (`npm --prefix gateway/cloudflare/worker ci`), and your ignored
`gateway/cloudflare/worker/wrangler.local.toml`. Wrangler must already be
signed in for account commands. Paths are resolved from the scripts, so the
current directory does not matter.

Run from the repository root:

```powershell
python scripts/cloudflare/check-cloudflare.py
python scripts/cloudflare/validate-cloudflare.py
python scripts/cloudflare/run-worker.py --dry-run
```

- `check-cloudflare.py` lists identity, VPC services, and tunnels without
  changing the account. Any failed command or timeout makes it exit nonzero.
- `validate-cloudflare.py` makes unauthenticated HTTP requests without
  following redirects. It checks `/` and `/ws` for single-host configs, or
  `/`, `/api/hosts`, and every configured `/ws/<id>` for multi-host configs
  (plus `/ws` when legacy migration is enabled). Each must return an HTTPS
  redirect to Cloudflare Access before the Wrangler dry run proceeds.
  This is a redirect smoke check, not proof of the owner policy, authenticated
  WebSocket behavior, or working remote control. It needs network access.
- `run-worker.py --dry-run` checks the existing build/config without
  publishing. To publish explicitly, use `run-worker.py --publish`.
  Neither mode builds assets or runs tests; follow the
  [build and deployment guide](../../docs/cloudflare-gateway.md#8-build-test-and-deploy)
  first. The existing npm deployment commands remain the normal workflow.

Console output redacts email addresses only. Raw Wrangler logs stay in
ignored `out/cloudflare/wrangler.log`; they can contain account details.
Do not share raw logs or commit local config backups, keys, or tokens.
Leave generated logs, PID files, and session handovers local under `out/`;
durable validation records belong in `specs/<feature>/validation.md`.

Offline regression checks (no credentials, network, or deployment):

```powershell
python -m unittest discover -s scripts/tests -p "test_cloudflare.py"
```
