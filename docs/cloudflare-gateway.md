# Browser access through a free workers.dev URL

Use TopVNC from your phone's browser to control your own computer, over the
internet, without opening a port, buying a domain, or installing a VPN on the
phone. Everything runs on Cloudflare's free plans. It is meant for one owner
and one host computer, with one browser session at a time.

**Status (2026-10-31).** The client, gateway, and Worker pass their local
tests. The owner has controlled a Windows desktop from iPhone Safari over the
live Cloudflare route: display, pointer, tap clicks, scrolling, pinch zoom,
on-screen typing, toolbar keys, and disconnect/reconnect. Returning from the
background, the two-hour soak, and Worker CPU within the Free limit are not
validated yet. See [Platform status](#platform-status).

- [How it works](#how-it-works)
- [Before you start](#before-you-start)
- [One-time setup](#one-time-setup) (about 30 minutes)
- [Pair your phone](#pair-your-phone) (once per browser)
- [Each session](#each-session)
- [Troubleshooting](#troubleshooting)

## How it works

Four programs take part. Three of them run on the host computer, each in its
own console window:

| Program | Runs on | What it does |
| --- | --- | --- |
| `topvnc --serve` | Host | Captures the desktop and takes input. Listens only on `127.0.0.1:5900`, with a VNC password. |
| `topvnc-web-gateway` | Host | Decrypts the browser's traffic and passes it to `topvnc --serve`. Listens only on `127.0.0.1:6080`. |
| `cloudflared` | Host | Opens an **outgoing** private tunnel to Cloudflare, so nothing on the host is exposed to the internet. |
| The Worker | Cloudflare | Serves the web page at `https://<name>.<subdomain>.workers.dev` and forwards its connection into the tunnel. |

```text
Phone browser ── HTTPS ──> <name>.<subdomain>.workers.dev   Cloudflare Access: only your email
                             ├─ /, /app.js, …   the web page (web/dist)
                             └─ /ws             the Worker forwards into the private tunnel
                                      ▼
                       cloudflared ── 127.0.0.1:6080 ── topvnc-web-gateway
                                                          │ end-to-end decryption
                                                          ▼ 127.0.0.1:5900
                                                    topvnc --serve (VNC password)
```

Three locks protect the session, and you meet each of them:

1. **Cloudflare Access sign-in.** Only your email address can open the page.
2. **Pairing.** The gateway accepts only browsers you have paired by scanning a
   QR code shown on the host's own screen. Cloudflare carries only encrypted
   data it cannot read. See [Pair your phone](#pair-your-phone).
3. **VNC password.** You type it in the page for every connection. It is sent
   inside the encrypted channel and never stored.

## Before you start

On the host computer (Windows is the tested host; the commands below are
PowerShell):

- **Rust** (stable), with the WebAssembly target:
  `rustup target add wasm32-unknown-unknown`
- **Node.js 22** with npm.
- **cloudflared**, on `PATH` or with its full path in the `CLOUDFLARED`
  environment variable. Do not install it as a Windows service; TopVNC starts
  it for you.
- A **Cloudflare account** with two-factor sign-in turned on.
- The **TopVNC repository**, built once with `cargo build --release`. Run every
  command below from the repository root unless a step says otherwise.

## One-time setup

You will collect a handful of values from Cloudflare and write them into two
local files that Git ignores. Nothing here is a password, but keep the files
on the host. This table is the checklist:

| Value | Where it comes from | `gateway/cloudflare/worker/wrangler.local.toml` | `gateway/cloudflare/gateway.local.toml` |
| --- | --- | --- | --- |
| Account ID | `npx wrangler whoami` (step 2) | `account_id` | |
| App URL | `https://<name>.<subdomain>.workers.dev` (step 3) | `APP_ORIGIN` | `app_origin` |
| Access team URL | `https://<team>.cloudflareaccess.com` (step 4) | `ACCESS_ISSUER` | `issuer` |
| Access AUD tag | The Access application (step 4) | `ACCESS_AUD` | `audience` |
| Your email | The Access allow rule (step 4) | `OWNER_EMAIL` | `owner_email` |
| VPC service ID | `wrangler vpc service create` (step 6) | `service_id` | |

The tunnel token (step 5) goes in its own file, not in either config.

### 1. Install the pinned build tools

These exact versions must match the lock files:

```powershell
# Repository root: bindings for the browser client.
cargo install wasm-bindgen-cli --version 0.2.128 --locked --root .tools

# Worker directory: the Worker packager and its bindings.
cd gateway/cloudflare/worker
cargo install worker-build --version 0.8.7 --locked --root .tools
cargo install wasm-bindgen-cli --version 0.2.129 --locked --root .tools
npm ci
cd ../../..
```

### 2. Sign in to Cloudflare

```powershell
cd gateway/cloudflare/worker
npm run login      # opens the browser for Wrangler sign-in
npx wrangler whoami
cd ../../..
```

`whoami` prints your **account ID**. In the Cloudflare dashboard, check that
Workers is on the **Free** plan and that you have a **workers.dev subdomain**.
Turn on **Zero Trust** and choose its free plan. Never select a paid upgrade.

### 3. Create the two config files

```powershell
Copy-Item gateway/cloudflare/worker/wrangler.example.toml gateway/cloudflare/worker/wrangler.local.toml
Copy-Item gateway/cloudflare/gateway.example.toml gateway/cloudflare/gateway.local.toml
```

In `wrangler.local.toml`, `name = "topvnc"` decides your **app URL**:
`https://topvnc.<your-subdomain>.workers.dev`. Choose the name now and do not
change it later, because the Access application protects that exact address.
Fill in `account_id` and the URL in both files; the other values come in the
next steps.

### 4. Protect the URL with Cloudflare Access

In the Zero Trust dashboard, add an **Access application** for the exact
hostname `topvnc.<your-subdomain>.workers.dev` (a self-hosted,
hostname-based application):

- one **Allow** policy that includes only your exact email address;
- no Bypass policy, and preview URLs off.

Do not use the "Cloudflare Access" toggle on the Worker's own settings page
instead: that option currently rejects the WebSocket connection the page needs
([Cloudflare Access for Workers](https://developers.cloudflare.com/workers/configuration/cloudflare-access/)).

Then copy two values into both config files:

- the **team URL**, `https://<team>.cloudflareaccess.com`, as the issuer;
- the application's **AUD tag**, as the audience.

Also set your email in both files. These values identify the application;
they are not login secrets.

### 5. Create the private tunnel

In the Zero Trust dashboard, create a **Cloudflare Tunnel** (cloudflared type)
for Workers VPC. Do **not** add a public hostname to it
([Workers VPC tunnel setup](https://developers.cloudflare.com/workers-vpc/configuration/tunnel/)).

1. Note the **tunnel ID**.
2. The dashboard shows an install command that ends with a long token. Copy
   only the token into `gateway/cloudflare/tunnel-token.txt`. That is
   where `cargo web-tunnel` looks for it. Do not run the install command.
   If you already set up the Phase 0 probe, move its
   `gateway/cloudflare/spike/tunnel-token.txt` there instead.
3. Restrict the file to your account:

   ```powershell
   icacls gateway/cloudflare/tunnel-token.txt /inheritance:r /grant:r "$($env:USERNAME):F" "SYSTEM:F"
   ```

The token lets anyone run your tunnel's connector. Keep it out of chats,
screenshots, and the browser.

The connector needs outbound UDP port 7844 (QUIC). Nothing inbound is opened.

### 6. Point a VPC service at the gateway

From `gateway/cloudflare/worker`, with your tunnel ID:

```powershell
npx wrangler vpc service create topvnc --type http --tunnel-id YOUR_TUNNEL_ID --ipv4 127.0.0.1 --http-port 6080
```

Copy the **service ID** it prints into `service_id` in `wrangler.local.toml`.
This is the only place the Worker can send traffic: the gateway on the host's
loopback port 6080.

### 7. Create the host key

```powershell
cargo run --release -p topvnc-web-gateway -- --init-host-key gateway/cloudflare/gateway.local.toml
icacls gateway/cloudflare/host.key /inheritance:r /grant:r "$($env:USERNAME):F" "SYSTEM:F"
```

It prints the **host key fingerprint**, such as `a1d1-…`. Your phone shows the
same fingerprint when it pairs. The command refuses to replace an existing
key. Never copy `host.key` off the host.

If you already ran the Phase 0 probe, you can skip this step and keep its key
and paired browsers instead: set `host_key_file = "spike/host.key"` and
`browser_keys_file = "spike/browsers.txt"` in `gateway.local.toml`, and keep
the probe's Worker `name`.

### 8. Build, test, and deploy

```powershell
cd web
npm ci
npm test
npm run build
npm run test:wasm
cd ../gateway/cloudflare/worker
npm run deploy:check   # dry run: builds everything and checks the config
npm run deploy         # publishes the page and the Worker
cd ../../..
```

**Check:** open the app URL in a private browser window. Cloudflare's Access
sign-in page should appear. If the TopVNC page appears without a sign-in, stop
and fix the Access application before going further.

## Pair your phone

Pairing introduces one browser to your host, once. The host shows a QR code;
scanning it gives the phone the host's public key and lets the host enroll a
new key that the browser creates. Afterwards the gateway accepts only enrolled
browsers (up to eight), and the phone refuses any host whose key has changed.
A QR code works once, for ten minutes. It must be shown only on the host's own
screen: never send the link through chat or email.

1. Start the server and the tunnel as in [Each session](#each-session), steps
   1 and 2.
2. In a third console, start the gateway **with a pairing code**:

   ```powershell
   cargo web-pair
   ```

   It prints the host key fingerprint, a QR code, and the link. Maximize the
   window so the QR code does not wrap.
3. On the phone, open the app URL and **finish the Access sign-in first**.
   The sign-in redirect loses the pairing part of the link, so scanning before
   signing in leaves the page at "Not paired".
4. Scan the QR code with the phone's camera and open the link. The page shows
   `Pairing invite for host …`. Check that this fingerprint matches the one in
   the gateway console.
5. Press **Connect** and enter the VNC password. The page says the browser is
   enrolled, and the console prints `enrolled browser key …`.

From now on, start the gateway with `cargo web-gateway` (no QR code). To pair
another browser, run `cargo web-pair` again.

## Each session

Open three console windows at the repository root and watch them yourself.
Start them in this order:

| Window | Command | Ready when it prints |
| --- | --- | --- |
| 1. Server | `cargo run --release -- --serve` | asks for a VNC password, then reports that it is serving on `127.0.0.1:5900` |
| 2. Tunnel | `cargo web-tunnel` | `Registered tunnel connection` |
| 3. Gateway | `cargo web-gateway` | `Gateway listening on 127.0.0.1:6080 for the RFB server at 127.0.0.1:5900` |

You can also start the server from the **Server** tab of the TopVNC window
instead of window 1; keep it on `127.0.0.1:5900` and set a password.

On the phone, open the app URL, sign in if asked, press **Connect**, and
enter the VNC password. Disconnect from the ☰ menu. When you are done, press
Ctrl+C in all three windows.

## Troubleshooting

| What you see | What to do |
| --- | --- |
| The TopVNC page opens without an Access sign-in | Stop. The Access application does not cover the exact app URL. Fix [step 4](#4-protect-the-url-with-cloudflare-access). |
| "Not paired" right after scanning | You scanned before signing in. Stay on the signed-in page and scan the same QR code again; it works until used or until ten minutes pass. |
| "The pairing invite expired or was already used" | Stop the gateway and run `cargo web-pair` for a new code. If you already paired, Safari may have reopened the old link: open the plain app URL instead. |
| "This browser is not enrolled on the host" | This browser was never paired, or its key was removed from the host. Pair it again. |
| "The private gateway is unavailable" | Window 2 or 3 is not running, or the tunnel has not printed `Registered tunnel connection` yet. |
| "TopVNC is not serving on the host" | Window 1 is not running, or listens on a different address than `rfb_target`. |
| "Another browser is connected" | Only one session runs at a time. Disconnect the other one, or wait about a minute if it was a phone that went to sleep. |
| "The pinned host key or the pairing secret does not match" | The host key changed. Pair this browser again, after checking the fingerprint. |
| "Your Cloudflare Access sign-in expired" | Reload the page and sign in. |
| `cargo web-tunnel` says the token file is not found | Save the token in `gateway/cloudflare/tunnel-token.txt` ([step 5](#5-create-the-private-tunnel)). |
| `cargo web-tunnel` says `cloudflared` is not found | Install cloudflared, or set `CLOUDFLARED` to its full path. |
| A build asks you to install a pinned tool | Run the `cargo install` command it prints, from the directory it names ([step 1](#1-install-the-pinned-build-tools)). |
| The QR code does not scan | Maximize the console and use a dark background. |

The gateway console logs why each session ended (`relay closed: code=…` or
`session rejected: …`). An abrupt disconnect, such as the phone locking, can
appear as `1011` there.

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
  file (`gateway/cloudflare/browsers.txt` by default), and restart. Forgetting
  the pairing in the browser does not remove it from the host.
- Rotate the host key: delete the host key file and run `--init-host-key`.
  Every browser must then pair again.
- Safari may delete a site's storage after seven days without a visit, unless
  the page was added to the Home Screen. The client then shows "Not paired":
  pair it again.

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

## Local test harness

To try the page without Cloudflare, build it (`npm run build` in `web/`), then
run `cargo run -p topvnc-web-gateway --features test-fixtures --example local_web`.
It serves the client at `http://127.0.0.1:8765`, standing in for the Cloudflare
edge with a test-signed Access JWT, and runs the real gateway and a
password-protected TopVNC server with a synthetic, animated scene. It prints a
pairing link, the test password, and the input the scene receives. It uses no
Cloudflare service and captures no desktop. Its signing key is public, so it
must stay on loopback.

## Platform status

| Path | Browser, device | Result |
| --- | --- | --- |
| Local harness, desktop client, Worker + OffscreenCanvas | Edge 156, Windows 11 | Pairing, VNC password, rendering, mouse, keyboard, wheel, served size, statistics, disconnect, reconnect, replacement, wrong password: passed (2026-10-31) |
| Local harness, page-thread fallback | Edge 156, Windows 11 (OffscreenCanvas removed) | Connect and render: passed (2026-10-31) |
| Live route, Phase 0 encrypted echo | iPhone, iOS 27.0.1, Safari; Edge 156 | Passed (2026-10-30); unenrolled browser rejected (2026-10-31) |
| Live route, desktop control of a Windows host | iPhone Safari | Owner-reported (2026-10-31): display, pointer, tap clicks, two-finger scroll, pinch zoom, on-screen typing, toolbar keys, disconnect and reconnect. Background return not confirmed; iOS version not recorded |
| Worker CPU within the Free plan's 10 ms | Workers analytics | Not passed: warm upgrades about 2 ms, but 7 of 88 cold upgrades used 13.0-18.7 ms (none terminated) |
| Two-hour soak, mobile link latency | iPhone Safari | Not tested |
| macOS or Linux host | Any | Not tested |
