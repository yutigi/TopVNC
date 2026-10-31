# Browser access through a free workers.dev URL

TopVNC's browser client lets the owner control their own host from a phone
browser through Cloudflare Free services: a `workers.dev` address, Cloudflare
Access, Workers Static Assets, Workers VPC (free during its beta), and a
private Cloudflare Tunnel. No domain, paid plan, phone VPN, or public RFB port
is needed. It is for one owner and one host, with one browser session at a
time.

**Status (2026-10-31).** The client, gateway, and Worker are implemented and
pass their local tests. A local harness runs the whole path in Edge on
Windows without Cloudflare (see [Platform status](#platform-status)). On the
live Cloudflare route, only the Phase 0 encrypted echo probe has run, in
iPhone Safari and Edge. Desktop control through Cloudflare, real-device touch
input, and the two-hour soak are not validated yet. Do not treat this page as
a claim that iPhone desktop control works.

```text
iPhone Safari ── HTTPS ──> topvnc.<you>.workers.dev        (Cloudflare Access: owner only)
                             ├─ /, /app.js, …  static assets (web/dist)
                             └─ /ws  Rust Worker: verifies the Access JWT, forwards the upgrade
                                      │ Workers VPC binding (one private HTTP service)
                                      ▼
                              private Cloudflare Tunnel ── outbound from the host
                                      ▼
                       cloudflared ── 127.0.0.1:6080 ── topvnc-web-gateway
                                                         │ verifies the JWT again, ends Noise E2E
                                                         ▼ 127.0.0.1:5900
                                                   topvnc --serve (VNC password)
```

## Security model

- **Access** protects the exact workers.dev hostname with a hostname-based
  application that allows only the owner's email. The Worker and the gateway
  both verify the `Cf-Access-Jwt-Assertion` JWT: signature, issuer, audience,
  expiry, and owner email. A decoded or unverified token is never trusted.
- **Fixed destinations.** The Worker forwards only `/ws` to its one VPC
  service. The gateway connects only to its configured loopback `rfb_target`.
  The browser cannot name a host, port, or path: query strings, other paths,
  other origins, and other subprotocols are rejected.
- **End-to-end encryption** (Noise `XK_25519_ChaChaPoly_BLAKE2s`, `snow`) runs
  between the browser and the gateway on the host. The Worker, VPC, and Tunnel
  carry only ciphertext: RFB credentials, frames, input, and clipboard are
  never readable at Cloudflare. The browser pins the host key from a QR code
  read off the host's own screen. The gateway accepts only enrolled browser
  keys (at most eight). A changed host key, an unknown browser key, or a
  tampered, replayed, reordered, or dropped message closes the session. There
  is no plaintext fallback.
- **VNC password authentication** still runs inside the channel, between the
  browser and `topvnc --serve`. The browser client refuses a server that offers
  no password. The password is used once per connection and never stored.
- **Trust limit (accepted by the owner on 2026-10-30).** Cloudflare serves the
  page's JavaScript and WebAssembly. Cloudflare, or anyone who controls the
  owner's Cloudflare account or Worker deployment, could serve modified code
  that reads the session, the typed password, and the browser's stored key. The
  page cannot detect this. Keep account 2FA on and sign Wrangler out when not
  deploying.
- **Bounds.** Each WebSocket message carries one Noise message with up to
  16 KiB of plaintext. The browser accepts framebuffers up to 16,777,216 pixels
  and buffers at most one maximal rectangle plus 256 KiB of read-ahead. The
  gateway relays one 16 KiB read at a time, with a five-second write deadline,
  so a browser that stops reading is disconnected instead of buffered. Sessions
  last at most `max_session_seconds` (up to 7200) and end at the Access JWT's
  expiry. A browser that stops answering the gateway's pings for 60 s, as a
  sleeping phone does, is disconnected.
- **Logging.** The gateway logs close reasons, byte counts, and key
  fingerprints. It never logs JWTs, passwords, pixels, input, or clipboard data.

## One-time setup

The Phase 0 steps in `gateway/cloudflare/spike/README.md` ("Account setup
before deployment", steps 1-7) create everything this needs:

- confirm Workers Free and Zero Trust Free;
- create the owner-only hostname Access application;
- create the private Tunnel and one HTTP VPC service for `127.0.0.1:6080`;
- store the connector token in a protected `tunnel-token.txt`.

The production Worker reuses the same Access application, VPC service, and
Tunnel. Keep the same Worker `name`, so the workers.dev hostname that Access
protects does not change.

1. Install the pinned build tools:

   ```powershell
   # Repository root: the browser client's bindings (same version as Cargo.lock).
   cargo install wasm-bindgen-cli --version 0.2.128 --locked --root .tools
   # gateway/cloudflare/worker: the Worker packager and its bindings.
   cargo install worker-build --version 0.8.7 --locked --root .tools
   cargo install wasm-bindgen-cli --version 0.2.129 --locked --root .tools
   ```

2. Copy `gateway/cloudflare/worker/wrangler.example.toml` to
   `wrangler.local.toml` and fill in the account, origin, Access issuer, AUD,
   owner email, and VPC service ID. Copy
   `gateway/cloudflare/gateway.example.toml` to `gateway.local.toml` and set
   the same origin, issuer, AUD, and owner email. Both files are ignored by Git.
3. Create the host identity once, unless you are keeping the Phase 0 one
   (`gateway.example.toml` explains how to reuse it):

   ```powershell
   cargo run --release -p topvnc-web-gateway -- --init-host-key gateway/cloudflare/gateway.local.toml
   ```

   On Windows, restrict the key file to your account, for example with
   `icacls host.key /inheritance:r /grant:r "$($env:USERNAME):F" "SYSTEM:F"`
   in PowerShell.

4. Build and check, then deploy only after the Free plans and owner-only
   Access configuration are confirmed:

   ```powershell
   cd web; npm ci; npm test; npm run build; npm run test:wasm; cd ..
   cd gateway/cloudflare/worker; npm ci; npm run deploy:check
   npm run deploy   # publishes web/dist and the Worker to workers.dev
   ```

## Each session

Run each long-lived command in its own console window that you watch
yourself. The pairing QR code and link carry a one-time secret.

1. Start TopVNC's server on the host. It asks for the VNC password and serves
   on `127.0.0.1:5900` unless you give another address:

   ```powershell
   cargo run --release -- --serve
   ```

2. Start the Tunnel connector (`cargo tunnel`) and wait for `Registered tunnel
   connection`.
3. Start the gateway: `cargo web-pair` the first time for each browser, and
   `cargo web-gateway` afterwards. It prints the host key fingerprint and
   `Gateway listening on 127.0.0.1:6080 for the RFB server at 127.0.0.1:5900`.
4. In iPhone Safari, open the workers.dev URL and finish the Access sign-in
   **before** scanning the QR code, because the Access redirect drops the
   `#pair=` fragment. Scan the code; the page shows the host fingerprint to
   compare with the console. Press **Connect**, then enter the VNC password.
   After the first successful connection the pairing is saved in this browser,
   and later visits need only **Connect** and the password.
5. Disconnect from the menu (☰). Stop the gateway, connector, and server with
   Ctrl+C when you are done.

## Using the viewer

- **Trackpad mode** (default): drag one finger to move the pointer and tap to
  click. Tap, then touch again and drag, to hold the left button. Tap with two
  fingers to right-click and drag with two fingers to scroll. A drawn arrow
  shows where touch input aims.
- **Direct mode**: the pointer goes where you touch. Tap to click there, drag
  to hold the left button, and long-press to right-click.
- **Zoom**: pinch with two fingers in either mode. **Fit** resets the view.
  Zoom changes only this device's view, not what the host sends.
- **Keyboard**: ⌨ opens the on-screen keyboard. Typed text arrives as text,
  with Shift added for capitals and shifted symbols on the host's US layout.
  Ctrl, Alt, Shift, and Win latch until the next key. **Fn** shows F1-F12 and
  navigation keys. A hardware keyboard sends physical keys, like the native
  viewer.
- **Mouse** (desktop browsers): absolute pointer, buttons, and wheel. Pointer
  Lock is used when the host asks for relative motion.
- **Menu**: picture quality, the host's served size and display (when
  offered), a statistics line (updates and frames drawn per second, Mbit/s,
  round trip through the host), and Disconnect.

Held keys and buttons are released when the page loses focus, is hidden, or
disconnects, and when a gesture is cancelled. Input typed while disconnected
is dropped, never replayed.

## Interruptions

- iOS closes the connection when Safari goes to the background or the phone
  locks. On return, the page says it was disconnected; press **Reconnect** and
  enter the password again.
- A reconnect from the same enrolled browser replaces its own stale session at
  once. A different browser gets "Another browser is connected" until the
  active session ends.
- When the Access sign-in has expired, the page says so; reload it to sign in
  again. When the gateway, connector, or host is down, the page names that
  instead.
- Cloudflare can close WebSockets during its own maintenance. That is reported
  as a lost connection, not hidden.

## Rotation and revocation

- Revoke a browser: stop the gateway, delete its line from the browser keys
  file, and restart. Forgetting the pairing in the browser does not remove it
  from the host.
- Rotate the host key: delete the host key file and run `--init-host-key`.
  Every browser must then pair again.
- Safari may delete a site's storage after seven days without a visit, unless
  the page was added to the Home Screen. The client then shows "Not paired":
  pair it again.

## Local test harness

`cargo run -p topvnc-web-gateway --features test-fixtures --example local_web`
(after `npm run build` in `web/`) serves the client at `http://127.0.0.1:8765`.
It stands in for the Cloudflare edge with a test-signed Access JWT and runs the
real gateway and a password-protected TopVNC server with a synthetic,
animated scene. It prints a pairing link, the test password, and the input the
scene receives. It uses no Cloudflare service and captures no desktop. Its
signing key is public, so it must stay on loopback.

## Platform status

| Path | Browser, device | Result |
| --- | --- | --- |
| Local harness, desktop client, Worker + OffscreenCanvas | Edge 156, Windows 11 | Pairing, VNC password, rendering, mouse, keyboard, wheel, served size, statistics, disconnect, reconnect, replacement, wrong password: passed (2026-10-31) |
| Local harness, page-thread fallback | Edge 156, Windows 11 (OffscreenCanvas removed) | Connect and render: passed (2026-10-31) |
| Live route, Phase 0 encrypted echo | iPhone, iOS 27.0.1, Safari; Edge 156 | Passed (2026-10-30); not desktop control |
| Live route, desktop control | Any | Not tested |
| Touch gestures, on-screen keyboard | iPhone Safari | Not tested on a device; gesture logic is unit-tested only |
| Two-hour soak, mobile link latency | iPhone Safari | Not tested |
