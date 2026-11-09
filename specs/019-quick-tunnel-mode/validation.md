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

## WEB-tab continuation and pairing URL copy (2026-11-07)

The continuation started from `d30576e` in the dedicated
`TopVNC-quick-tunnel` worktree. PR #10 was still open at that head; its Format,
Windows Rust, Ubuntu Rust, and browser/Worker CI checks were successful.
The project constitution is still an unfilled template; AGENTS.md applies.

Built matching binaries with `cargo build --workspace --locked` and rebuilt
the page with `node web/scripts/build.mjs` (run from `web/`). The build used
the worktree's sources and target directory, wasm-bindgen 0.2.128, and
cloudflared 2026.10.3. No main-checkout binaries or assets were used.

Actual Windows WEB-tab observations, before the copy-button change:

- Access was the default. Selecting Quick Tunnel displayed its pairing,
  password, temporary-URL, and validation-limit explanations.
- With a test-owned loopback HTTP listener occupying port 6080, Start
  reported gateway address-in-use. Server and Tunnel returned to Stopped;
  no cloudflared or gateway child remained. No public route was launched.
- After releasing that listener, Start showed the gateway's
  `bound 127.0.0.1:6080` message while Tunnel was Starting. The real connector
  then registered one connection, both children became Ready, and the panel
  displayed the temporary URL and Copy app URL control.
- An earlier startup was stopped after noticing that bulk automated typing
  had entered only one password character. It is not authentication or viewer
  evidence. Subsequent startup used eight individually entered characters,
  verified by the masked field. All test app/connector/gateway processes from
  those runs exited before opening the revised build.

The owner reported that the pairing URL could not be copied. The QR-only
panel intentionally had no such action. At the owner's request the revised
panel adds **Copy pairing URL**, retaining the invite only in memory while
active. It copies the same fragment-bearing URL as the QR, refuses expired
invites, and does not display or log the URL. Debug formatting omits both the
link and the secret-bearing QR modules. Enrollment, cancellation, expiry,
failure, and shutdown retain their existing invite-removal behavior. The OS
clipboard copy remains until replaced and does not extend invite validity.

Checks for the copy-button revision:

- `cargo fmt --all --check`: passed.
- `cargo test --workspace`: 349 passed, 7 existing tests ignored, with UTF-8
  console input/output. Regression checks cover copying the active invite,
  expiry at its deadline, redacted Debug output, pending pairing, enrollment,
  route mismatch, and removal on stop.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- Rebuilt browser assets: JavaScript tests 25 passed; WASM tests 8 passed.
  The copy action changes only the native panel and its invite state.
- `git diff --check`: passed.

The revised build was opened for the owner's copy/phone check. The owner
confirmed that the copy action worked on 2026-11-07. This confirmation alone
does not establish a complete Safari session. Real desktop display/input,
Safari camera pairing, background/resume,
reconnect, public-URL shutdown/restart, and live gateway-failure supervision
are still pending in this continuation. Startup readiness alone is not
evidence that the desktop was displayed or controlled. The older synthetic
Chromium run remains separate evidence and predates both review revisions.

## Limitations popup (2026-11-07)

The owner confirmed testing the implemented popup in the next continuation.
The local build passed formatting, 351 Rust tests (7 existing ignored),
Clippy with warnings denied, and `cargo build --workspace --locked`.
Earlier agent UI checks observed Start and Enter opening the modal, blocked
underlying controls, and Escape/default Enter cancelling without closing
TopVNC or starting children. The owner stopped Computer Use with Escape
before the final live startup/cleanup check; that check was not completed.
The popup confirmation is not evidence of email-gated WebSockets or iPhone
background/resume behavior.

## Repeatable live checks and native failure cleanup (2026-11-07)

Added `scripts/check-quick-tunnel.mjs`, with a bounded loopback-only synthetic
RFB fixture, mandatory disposable VNC authentication, isolated cloudflared
configuration/environment, bind-before-publish, and connector-first shutdown.
Browser login uses a private disposable Chromium session. Reports contain
only selected outcomes and timings, not addresses, PINs, cookies, pairing
storage, passwords, raw logs, or saved screenshots. Capture verification
uses synthetic viewer images only in memory.

The completed pairing-only run at 09:39:59-09:41:56 UTC passed:

- Real Quick Tunnel HTTP 101, mandatory pairing/password, matching host
  fingerprint, and changing rendered synthetic frames.
- Key and pointer input reached only the synthetic server. A browser blur
  released the held key/button at that server.
- Chromium page freezes of 2 and 65 seconds each disconnected. Both resumed
  with an enrolled reconnect and new frames, without another invite.
- A second run created a different URL and host identity. The prior pairing
  was rejected; fresh pairing, frames, input release, and reconnect passed.
- Connector-first stop disconnected the browser and made the old public URL
  unavailable. All owned child processes exited and scratch profiles were
  removed. This is direct harness evidence, not real-desktop WEB-tab evidence.

Separately, the actual native Windows WEB tab passed the popup Continue/start
flow with eight individually entered disposable password characters. The
gateway bound before connector registration; all rows became Ready. Its
public page returned 200. Only the gateway identified as a child of this
test-owned native client was terminated. The connector exited, the desktop
listener on 5900 closed, and the old public URL became unavailable. The panel
showed Gateway Failed with Server/Tunnel Stopped and restored Start/Pair
controls. No pairing invite was issued or captured in this native run.
Failure supervision while a native viewer is active still needs a live check.

The owner subsequently reported "test success" after entering the email PIN.
The email-gated harness run at 09:44:31-09:45:45 UTC independently recorded:

- Before login, page, app asset, and `/ws` requests all returned 302.
- After owner sign-in, the authenticated upgrade returned 101 and the
  encrypted synthetic session, safe input release, and enrolled reconnect
  passed. Fresh clients still received 302 for page, asset, and `/ws`.
- The earliest recorded browser-cookie expiry was about five seconds away.
  The existing encrypted session stayed connected after that time. Replaying
  captured cookies redirected page/asset requests but the WebSocket rejection
  assertion failed. That first report did not save the failing response status.
- Cleanup completed with no owned children remaining. Cookie-removal and
  normal shutdown assertions were not reached before this failure.

This is partial success, not an email-support claim. The harness is being
tightened to inspect the specific cloudflared auth cookie and its expiry
claim, use independent probes with random handshake keys, and retain only
status-code diagnostics even on failure. Cloudflare's
[session implementation](https://github.com/cloudflare/cloudflared/blob/2026.10.3/quicktunnelauth/session.go)
caps sessions at the earlier of identity expiry and four hours. A
[public upstream issue](https://github.com/cloudflare/cloudflared/issues/1752)
reports roughly five-second protected sessions; it corroborates the short
lifetime symptom but does not establish this run's cause or explain the
WebSocket replay result. Product email configuration remains deferred.

Current local gates: formatting, Rust workspace (351 passed, 7 existing
ignored), Clippy with warnings denied, native build, rebuilt web assets,
25 JavaScript and 8 WASM tests passed. The new live-harness regression tests
cover launch isolation, refusal to classify service failures as login gates,
auth-cookie selection/redaction, the DES challenge vector, and bad-password
rejection. Physical iPhone results still require device/version and concrete
observations; neither Chromium freezes nor the owner's general confirmation
establishes them.

### Email retry with explicit auth-cookie inspection

The owner completed the fresh private-browser login in the
10:01:57-10:05:00 UTC run and observed the window closing automatically.
Cleanup closed that window after an assertion failed; it was not an
all-checks-passed signal. All owned processes exited.

This retry inspected exactly one
`__Host-cloudflared-qt-auth-session` cookie. Its browser expiry and parsed
signed expiry claim agreed (within the browser's fractional timestamp), with
four seconds remaining immediately after app load and two at the authenticated
upgrade. This confirms that the short lifetime belongs to the actual auth
cookie, rather than a different login-state cookie. The parser inspects the
expiry claim; it does not independently verify cloudflared's HMAC signature.

Before login, page/asset/WebSocket probes returned 302. Authenticated upgrade
returned 101; encrypted synthetic frames, input release, and enrolled
reconnect passed again. The subsequent fresh-client WebSocket rejection check
failed before natural-expiry and cookie-removal stages. Its exact response
status was not retained, so this record does not establish an authentication
bypass or an expired-cookie rejection result.

The harness now preserves allowlisted status diagnostics before validating
every gate probe and collects later negative checks even if one fails. A
failed negative check still makes the overall run fail; this is not a retry
without email protection. Six harness regressions pass, including failed
assessment diagnostics without leaking response bodies or redirect secrets.
The corrected diagnostic run completed below.

### Completed email-gate diagnostic run

The owner completed sign-in for the 10:08:27-10:12:29 UTC run. All
implemented checks passed with cloudflared 2026.10.3 and a disposable private
Chromium browser:

- Before login, `/`, `/app.js`, and `/ws` each returned 302 with a login
  location. After login, the authenticated upgrade returned 101, the paired
  and password-authenticated synthetic viewer rendered changing frames, key
  and pointer input released on blur, and enrolled reconnect passed.
- A fresh client after login still received 302 for all three paths.
- The actual auth cookie had four seconds remaining just after app load and
  two at the authenticated upgrade. The parsed expiry claim agreed with its
  browser expiry. After that deadline, replaying the original cookies gave
  302 with a login location for page, asset, and WebSocket alike.
- The existing encrypted session remained connected after cookie expiry.
  This does not establish revocation of an already-open connection; TopVNC's
  per-connection cap remains separate from email admission.
- Clearing the disposable browser's cookies blocked an actual browser
  WebSocket attempt. Fresh HTTP/asset/upgrade probes again returned 302.
  This cookie-removal result is separate from the timed expiry replay above.
- Normal shutdown made the old public app URL unavailable. The browser,
  connector, gateway, and fixture were closed; zero owned children remained,
  scratch cleanup succeeded, and the harness exited successfully.

The report is `target/quick-tunnel-checks/1794046107039/report.json` (ignored,
local evidence). It contains status codes and selected expiry metadata only,
not email, PIN, tokens, invite, password, or browser keys. Earlier failed
assertions remain part of the record; their missing status codes prevent a
root-cause conclusion, and they were not reproduced in this completed run.

This establishes the tested Windows Chromium email-gated path and rejection
behavior, not reliable login duration or physical iPhone behavior. The short
observed auth lifetime makes reload and new connections require email sign-in
again very quickly. The WEB tab's optional email field remains deferred for
that usability limit; the harness continues to require the email flag without
an ungated fallback. No product authentication behavior was changed.
