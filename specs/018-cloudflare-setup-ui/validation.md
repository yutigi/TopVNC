# Validation

## Phase 1: Run view, 2026-11-04

Windows 11, main checkout on `feat/cloudflare-web-client` (base `b9750a8`).
Rust and Clippy 1.99.0; cloudflared 2026.10.3.

Repository gates:

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- `cargo test --workspace`: pass, 306 passed and the 7 existing manual or
  benchmark tests ignored. No test was excluded.
- `git diff --check`: pass.

New automated tests:

- Gateway `--supervised` mode, with the real `topvnc-web-gateway` binary and
  a scratch config and host key: `fingerprint` and `ready` lines, `pair`
  returning the configured origin with `#pair=`, `cancel-pair`, an unknown
  command ignored, a clean exit after stdin closes, and no `#pair=` on
  stderr. Command parsing unit test.
- Config checks: a complete config; the example file refused (step 3, then
  step 4 once the origin is filled in); trailing slash, uppercase, and
  non-workers.dev origins; a bad issuer and placeholder or invalid email;
  non-loopback `listen` or `rfb_target`; port 0; a session cap outside 1-7200;
  unknown and missing fields; `CLOUDFLARED` taking precedence over `PATH`.
- Line rules: the gateway protocol (including malformed `pair` lines),
  `enrolled browser key`, cloudflared's registered, unregistered, terminated,
  and lost-connection lines; removal of every `#pair=` secret before a line
  is cut or kept.
- Supervised process: bounded output and event queue, line splitting and
  cutting, a real PowerShell child (stdin, stdout, stderr, exit code).
- SC-002: a helper process starts a child through the same spawner and is
  then killed with no chance to clean up; the child ends through the Windows
  job. With the job disabled, the same test fails ("the child outlived the
  killed parent"), so it does test the job.
- Runtime state machine with simulated programs: start order (server, then
  tunnel, then gateway), the token passed only as a file path, stop order
  (gateway stdin closed and up to 3 s, then the tunnel, then the server only
  if the WEB tab started it), start time limits, child exits and spawn
  errors, no automatic restart, server reuse rules (address, password, plain
  RFB), the server stopping elsewhere, tunnel reconnects, pairing before and
  after the gateway is ready, enrollment, expiry, Cancel (including an invite
  that arrives after Cancel), `pair-error`, and the code removed on stop or a
  gateway failure. Dropping the runtime ends its children.
- QR code: the drawn modules match the `qrcode` crate's own rendering module
  for module, and a typical link fits within version 10.
- WEB tab layout bounds and the shared, unsaved password; the WEB server
  request is plain RFB on loopback with a password, never All networks or no
  password, keeps the Server tab's other choices, and brackets IPv6.

Rendering: the idle, running-and-pairing, and failed states were rendered
offline to images with simulated programs and checked by eye. That render
helper was temporary and is not in the repository.

## Not yet validated

- **T009, owner live check:** Start from the WEB tab with the real
  configuration, a phone session over the live route, Pair a browser with a
  phone, Stop, closing TopVNC, and killing TopVNC from Task Manager, then
  checking that no `topvnc-web-gateway` or `cloudflared` process remains
  (SC-001, SC-002). This starts the real tunnel connector against the
  owner's Cloudflare account, so it was not run without the owner.
- cloudflared's reconnect lines (`Unregistered tunnel connection`,
  `Connection terminated`, `Lost connection with the edge`) were taken from
  the 2026.10.3 binary, not observed in a live reconnect.
- macOS and Linux: not built or run. There, children end through the
  gateway's stdin and TopVNC's own shutdown, not a job object; a TopVNC crash
  can leave cloudflared running.
- Phase 2 (Setup view) is not started.
