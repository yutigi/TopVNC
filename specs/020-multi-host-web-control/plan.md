# Implementation plan

Keep routing at the Worker and protocol/E2E logic unchanged. A bounded
`HOSTS_JSON` registry maps IDs and labels to environment service bindings and
validated private upstream URLs. The same owner JWT policy protects the
registry and upgrades; the gateway repeats authentication on `/ws`.

The page fetches public registry metadata, validates it, and uses separate
pairing storage keys. Only `LEGACY_HOST_ID` authorizes migration of v1 storage.
Invites keep the secret in `#pair=` and put the non-secret machine ID in
`?host=`. The gateway validates the optional ID; the WEB tab mirrors it.

Each browser session uses a fresh execution context and canvas. A session
slot invalidates old callbacks immediately, requests close/release, and waits
for teardown acknowledgement before another session starts. Async setup and
close diagnostics check their generation before changing page state.

Validate allowlist/auth failures and metadata disclosure with Worker fixtures;
exercise isolation, migration, invite policy, and session replacement using
Node tests. Run Rust and browser checks, and document unperformed live checks.
