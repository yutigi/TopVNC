# Quick Tunnel validation

## Environment

Windows 11, `feat/quick-tunnel-mode`, based on `d29557f`. Local dates are
Asia/Bangkok (UTC+07:00). cloudflared 2026.10.3; HeadlessChrome 153.0.8010.12
from the installed Playwright Chromium 1243 build. The browser uses the
built production page and its real Worker/WASM session through Cloudflare.
The gateway runs in `--supervised-quick-tunnel` mode and serves `web/dist`.

A separate synthetic RFB 3.8 server listens only on `127.0.0.1:5900`. It
requires VNC authentication and sends a moving 640x360 raw test pattern.
The test creates a disposable password and browser profile. No real desktop,
Cloudflare account credentials, or saved TopVNC password is used.

## Live checks

On 2026-11-06 (2026-11-05 UTC), the previous session captured a real
cloudflared startup and checked page delivery and WebSocket origin rules.
The continuation repeated those checks and completed a Chromium session:

- `/` returns 200 with CSP, `X-Frame-Options: DENY`, no-referrer, and nosniff.
  `/host.key` returns 404; the WASM is served as `application/wasm`.
- An upgrade with the exact tunnel origin returns 101. Another tunnel's
  origin, the origin with explicit `:443`, an HTTP origin, and no Origin
  each return 403. A browser upgrade also succeeds with its normal Origin.
- A fresh invite enrolls the browser; the encrypted session prompts for the
  synthetic VNC password and displays the 640x360 test pattern. The invite
  fragment and the password input are cleared. Pairing persists locally.
- A ten-minute continuous session remained connected at all 40 samples
  (every 15 seconds), with nonzero frame-update rates throughout. Captured
  images confirmed changing rendered pixels. This is a synthetic raw-frame
  stream, not an input-latency measurement or a two-hour soak.
- Disconnect and reconnect with the enrolled browser work without another
  invite; the VNC password is requested again.
- Restarting cloudflared and the gateway gives a different URL and host key
  fingerprint. The new origin has no saved pairing. Deliberately copying the
  previous pairing into that origin fails to connect; a fresh invite then
  pairs and displays frames. Reconnect works in the second run too.
- After each stop, the browser disconnects and the old public page is no
  longer reachable (the test fetch fails). All test-owned child processes
  exit, including the gateway, connector, synthetic server, and browser.

The first restart attempt completed pairing and displayed frames, but a
cropped-image comparison missed the moving bar. The short two-run check was
repeated with full-viewer captures and passed, including both reconnects and
shutdowns. The completed ten-minute session is separate evidence.

## Regression fixture

`src/web_access/fixtures/cloudflared-quick-tunnel.txt` is the startup captured
by the previous session, including the banner, QUIC/HTTP2 prechecks, and the
single registered connection (`ha-connections:1`). Machine-specific private
addresses and UUIDs are replaced with documentation addresses and redaction
markers. There are no credentials or pairing invites in the fixture.

The line-parser test consumes the capture on both stdout and stderr and
expects exactly one Quick Tunnel URL, followed by `Registered(0)`. Other
links and the successful connectivity prechecks must not register the
tunnel. Existing tests retain malformed-origin cases and cover waiting for
both the URL and registration before starting the gateway.

## Repository checks

- `cargo fmt --all --check`: passed.
- `cargo test --workspace`: 327 passed, 7 existing manual/benchmark tests
  ignored; passed with UTF-8 console input/output encoding.
  The first run using this machine's Thai console code page failed the
  existing Unicode saved-password round-trip test; the full UTF-8 rerun
  passed without changing password storage or excluding any test.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `npm test` in `web/`: 25 passed.
- `npm run test:wasm` in `web/`: 8 passed against the built WASM bindings.
- `git diff --check`: passed.

## Remaining validation limits

- iPhone Safari: camera/QR pairing, input, background/resume, and reconnect
  through a Quick Tunnel have not been tested.
- The real desktop flow from the WEB tab, including its displayed QR code
  and starting/stopping all three components, has only local simulated
  coverage. The live test supervises the real gateway and tunnel directly.
- A two-hour/high-bitrate session has not been tested. A shorter synthetic
  stream cannot establish long-term throttling behavior or uptime.
- macOS and Linux have not been built or run for this feature.

These results establish the tested Windows Chromium route only. They do
not establish phone or cross-platform support or a latency improvement.

## Review fixes (2026-11-07)

PR #10 review found that inherited cloudflared ingress could bypass the
gateway, an occupied gateway port could be published before its bind failed,
and reopening the landing window could label an active Quick Tunnel as Access.
The revised flow binds the gateway before starting cloudflared, supplies a
per-run configuration with exactly that ingress, and removes inherited
`TUNNEL_*` options from this child. The origin arrives once on stdin; the
gateway keeps the same listener throughout startup. Shutdown stops forwarding
first, including on failures. Supervision continues during native viewer
sessions, and a reopened landing window restores the active route.

Local Windows validation:

- `cargo fmt --all --check` and
  `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: 348 passed, 7 existing tests ignored;
  `npm test` in `web/`: all 25 passed.
- The real supervised gateway holds its listener before receiving an origin,
  admits no HTTP in that phase, serves on the same socket after configuration,
  refuses occupied ports and invalid origins, and exits on stdin EOF.
- Runtime regression tests cover separate bind/origin readiness deadlines,
  stopping on gateway failure, ordering connector termination before gateway
  shutdown, generated-config cleanup, and active-route restoration.
- With cloudflared 2026.10.3 and a disposable user profile, the original
  `--url` invocation selected a conflicting user-config service on port 18080.
  Adding the generated configuration selected the intended port 6080. The
  `ingress rule` check created no public tunnel.

The earlier live Chromium record precedes these review fixes. This revised
startup/shutdown flow has not been rerun through a real Quick Tunnel or the
real-desktop WEB tab. The phone, platform, and long-session limits above remain.
