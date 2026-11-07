# Shared browser control for multiple machines

Date: 2026-11-07. Status: local implementation; live validation pending.

## Scope

One owner uses one Access-protected Cloudflare website to select among a
static registry of at most 32 machines. Each machine retains its own gateway,
private tunnel, host identity, browser enrollment, and VNC password. The
browser controls one machine at a time. No discovery, fleet administration,
simultaneous viewers, deployment, or native transport changes are included.

## Acceptance criteria

- A protected, non-cacheable `/api/hosts` response contains only stable IDs,
  display names, and the explicit legacy migration target, never upstreams,
  bindings, credentials, or keys.
- `/ws/<id>` routes only to the configured private binding and fixed `/ws`
  upstream. Unknown/malformed IDs, query strings, wrong origins, unauthenticated
  requests, and non-binary upgrades never reach a gateway.
- Existing `HOST_NET` / `UPSTREAM_URL` deployments and `/ws` remain valid.
  Explicit registries require `/ws/<id>`; no implicit default route exists.
- Pairings are isolated by stable ID. Legacy storage is read only in legacy
  mode or explicitly migrated to one configured `LEGACY_HOST_ID`. Changing
  selection cannot copy keys. A changed pinned host key requires forgetting
  that machine's pairing before accepting another invite.
- Machine-specific QR links select their configured machine. Unknown IDs and
  unscoped invites in explicit registry mode fail closed without altering keys.
- Switching releases held input, exits pointer lock, clears passwords, pending
  invites, capabilities, and old pixels, and closes the previous session before
  another can start. Stale messages and asynchronous diagnostics cannot update
  the new selection or save keys into its pairing slot.
- Gateway CLI and native WEB supervision generate machine-specific invites
  when `host_id` is set; old configuration files remain valid.
- Focused JavaScript/Rust tests, the browser build, WASM tests, workspace tests,
  formatting and Clippy are run. Documentation distinguishes local evidence
  from live Cloudflare/browser validation.
