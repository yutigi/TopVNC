# Browser access through a free workers.dev URL

Use TopVNC from your phone's browser to control your own computer, over the
internet, without opening a port, buying a domain, or installing a VPN on the
phone. Everything runs on Cloudflare's free plans. It is meant for one owner
and one host computer, with one browser session at a time.

**Status (2026-11-01).** The client, gateway, and Worker pass their local
tests. On 2026-10-31 the owner controlled a Windows desktop from iPhone Safari
over the live Cloudflare route: display, pointer, tap clicks, scrolling, pinch
zoom, on-screen typing, toolbar keys, and disconnect/reconnect. Returning from
the background and the two-hour soak are not validated yet, and Worker CPU is
an open **risk** on the Free plan (see [Free-plan Worker CPU](#free-plan-worker-cpu)).
See [Platform status](#platform-status).

**This runbook has not been run end to end.** The live deployment was built
step by step during the Phase 0 probe, under the probe's names, and then
switched to the production client. On 2026-11-01 each step below was checked
against the repository and the current tools, locally and with Wrangler dry
runs, without changing the Cloudflare account. Every step carries a
**Verified** note saying how. [Runbook verification](#runbook-verification)
lists what still needs a live run by the owner.

- [How it works](#how-it-works)
- [Before you start](#before-you-start)
- [One-time setup](#one-time-setup) (about 30 minutes)
- [Pair your phone](#pair-your-phone) (once per browser)
- [Each session](#each-session)
- [Quick Tunnel (temporary, no account)](#quick-tunnel-temporary-no-account):
  a temporary URL with no setup, protected only by pairing and the VNC
  password; phone and long-session checks pending
- [Troubleshooting](#troubleshooting)
- [Runbook verification](#runbook-verification)

## How it works

Four programs take part. Three of them run on the host computer, either in
three console windows or from TopVNC's **WEB** tab
([Each session](#each-session)):

| Program | Runs on | What it does |
| --- | --- | --- |
| `topvnc --serve` | Host | Captures the desktop and takes input. Listens only on `127.0.0.1:5900`, with a VNC password, as explicit plain RFB (`--unencrypted`): the gateway's encrypted channel protects it, and it never leaves loopback. |
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
- **Node.js 22** with npm (checked with Node 22.18.0 and npm 11.6.0).
- **cloudflared**, on `PATH` or with its full path in the `CLOUDFLARED`
  environment variable. Do not install it as a Windows service; TopVNC starts
  it for you. It needs `tunnel run --token-file`, present in 2026.10.3, the
  version used live.
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

The browser client's bindings must match the version in `Cargo.lock`. The
Worker is plain JavaScript and needs only Wrangler:

```powershell
# Repository root: bindings for the browser client.
cargo install wasm-bindgen-cli --version 0.2.128 --locked --root .tools

# Worker directory: Wrangler.
cd gateway/cloudflare/worker
npm ci
cd ../../..
```

The browser client's build refuses to run with a missing or different tool
and prints the `cargo install` command to fix it.

**Verified 2026-11-01, locally:** the version matches `Cargo.lock` (0.2.128);
the installed tool and both `npm ci` runs worked. The Worker was Rust
compiled to WebAssembly until 2026-11-01 and needed two more pinned tools;
see [Free-plan Worker CPU](#free-plan-worker-cpu).

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

Wrangler's sign-in can lack permission to read billing or Access settings
(HTTP 403). That does not mean the services are missing; check them in the
dashboard instead.

**Verified:** `whoami` with Wrangler 4.155.0 on 2026-11-01. Workers Free and
Zero Trust Free were confirmed in the dashboard by the owner on 2026-10-30.

### 3. Create the two config files

```powershell
Copy-Item gateway/cloudflare/worker/wrangler.example.toml gateway/cloudflare/worker/wrangler.local.toml
Copy-Item gateway/cloudflare/gateway.example.toml gateway/cloudflare/gateway.local.toml
```

In `wrangler.local.toml`, `name = "topvnc"` decides your **app URL**:
`https://topvnc.<your-subdomain>.workers.dev`. Choose the name now and do not
change it later, because the Access application protects that exact address.
Fill in `account_id` (Wrangler file only) and the app URL (both files); the
other values come in the next steps. Replace every placeholder: the gateway
refuses to start unless `app_origin` is an exact, lowercase
`https://….workers.dev` origin with no trailing slash.

**Verified 2026-11-01, locally:** every key in the two example files is one
the Worker (`APP_ORIGIN`, `ACCESS_ISSUER`, `ACCESS_AUD`, `OWNER_EMAIL`,
`UPSTREAM_URL`, binding `HOST_NET`) or the gateway reads. The Worker uses no
secrets, so there is no `wrangler secret put` step.

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

**Verified live by the owner, 2026-10-30,** for the Phase 0 hostname: the
application with one exact-email Allow rule and a six-hour session saved in
the dashboard. Rechecked 2026-11-01 without signing in: `/` and `/ws` on the
deployed hostname both answer 302 to the team's Access login. A signed-in
wrong email has not been tried.

### 5. Create the private tunnel

In the Zero Trust dashboard, create a **Cloudflare Tunnel** (cloudflared type)
for Workers VPC. Do **not** add a public hostname to it
([Workers VPC tunnel setup](https://developers.cloudflare.com/workers-vpc/configuration/tunnel/)).

1. Note the **tunnel ID**. `npx wrangler tunnel list` (experimental, read
   only) also shows it, with the tunnel's status. `cloudflared tunnel list`
   does not work for a tunnel made in the dashboard unless you also ran
   `cloudflared tunnel login`, which this setup does not need.
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

**Verified live, 2026-10-30,** as the Phase 0 tunnel, created in the dashboard
and run from `gateway/cloudflare/spike/tunnel-token.txt` with `cargo tunnel`.
`cargo web-tunnel` and the `gateway/cloudflare/tunnel-token.txt` path have
been checked only locally (2026-11-01): without the file it stops with
`gateway/cloudflare/tunnel-token.txt not found`, before starting
cloudflared.

### 6. Point a VPC service at the gateway

From `gateway/cloudflare/worker`, with your tunnel ID:

```powershell
npx wrangler vpc service create topvnc --type http --tunnel-id YOUR_TUNNEL_ID --ipv4 127.0.0.1 --http-port 6080
```

Copy the **service ID** it prints into `service_id` in `wrangler.local.toml`.
This is the only place the Worker can send traffic: the gateway on the host's
loopback port 6080. If your sign-in reaches more than one Cloudflare
account, Wrangler asks which to use.

If you already set up the Phase 0 probe, do not create a second service:
`npx wrangler vpc service list` shows the existing one (`topvnc-spike`), and
its ID goes in `service_id`.

**Verified:** the flags against `wrangler vpc service create --help` in
Wrangler 4.155.0 (2026-11-01). The same command, named `topvnc-spike`, created
the live service on 2026-10-30; `vpc service list` shows it as HTTP, port
6080, host 127.0.0.1.

### 7. Create the host key

```powershell
cargo run --release -p topvnc-web-gateway --bin topvnc-web-gateway -- --init-host-key gateway/cloudflare/gateway.local.toml
icacls gateway/cloudflare/host.key /inheritance:r /grant:r "$($env:USERNAME):F" "SYSTEM:F"
```

It prints the **host key fingerprint**, such as `a1d1-…`. Your phone shows the
same fingerprint when it pairs. The command refuses to replace an existing
key. Never copy `host.key` off the host. The key file paths in
`gateway.local.toml` are relative to that file.

**Verified 2026-11-01, locally,** with a scratch copy of the config: the key
was created, a second run refused to replace it, and `icacls` left only the
user and SYSTEM. (`--bin` is required: without it, `cargo run` stops because
the package has two programs.)

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
npm run deploy:check   # dry run: Worker tests, page build, config check
npm run deploy         # the same, then publishes the page and the Worker
cd ../../..
```

**Check:** open the app URL in a private browser window. Cloudflare's Access
sign-in page should appear. If the TopVNC page appears without a sign-in, stop
and fix the Access application before going further.

`compatibility_date` must not be later than today in UTC; Cloudflare rejected
a future date during the first live deploy (API error 10021).

**Verified:** on 2026-11-01, locally, everything before `npm run deploy`:
`npm ci`, `npm test` (9 pass), `npm run build`, `npm run test:wasm` (2 pass),
and `npm run deploy:check` with the owner's config, plus
`npx wrangler deploy --dry-run --config wrangler.example.toml`. Both dry runs
read 6 assets and list the `HOST_NET` VPC binding and the five variables.
With the JavaScript Worker, `deploy:check` also runs the Worker's tests
(17 pass) and the upload is 14.01 KiB (4.71 KiB gzip); the Rust Worker's was
649.51 KiB. `npm run deploy` itself last ran live on 2026-10-31 (versions
`9e792e97` and `31ffb4b5`), with the Rust Worker and the Phase 0 Worker name.

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

   With the WEB tab instead of consoles, press **Pair a browser**. TopVNC
   draws the QR code, the fingerprint, and the minutes left in its own window,
   and removes the code when a browser enrolls, after ten minutes, or on
   **Cancel pairing**. **Copy pairing URL** copies the active invite so you can
   open it directly in your browser. Keep that clipboard content private;
   it remains there until replaced, but expires with the invite. The link
   is never shown as text or logged. Pairing this way does
   not restart the gateway, so a connected phone stays connected.
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

**Verified:** pairing through **Connect** with the production gateway, in Edge
on the local test harness (2026-10-31). On the live route the owner's iPhone
was paired by the Phase 0 probe's `cargo pair`, whose keys the production
gateway reuses; the sign-in-before-scanning rule above comes from that live
run. `cargo web-pair` has not yet paired a browser over the live route.

## Each session

### From the WEB tab

TopVNC can run all three programs itself. Build the gateway next to TopVNC
once, with the same profile you run TopVNC with:

```powershell
cargo build --release -p topvnc-web-gateway
cargo run --release
```

The tab's **ACCESS** and **QUICK TUNNEL** buttons choose the route for the next
start. This section describes **ACCESS**, the default; see
[Quick Tunnel](#quick-tunnel-temporary-no-account) for the other.

On the **WEB** tab, type the VNC password and press **Start browser access**.
The Server, Tunnel, and Gateway rows turn **Ready** in that order, on the same
ready signals as the console table below. TopVNC serves this desktop as plain
RFB on the gateway's `rfb_target` (loopback) with your password, using the
Server tab's display, size, mouse, and priority choices; it never uses All
networks or no password, and `--unencrypted` is not needed on the command
line. It runs cloudflared and the gateway without console windows; click a
row to see its recent output. **Stop browser access**, or closing TopVNC, ends
all three. On Windows the children are tied to TopVNC, so they also end if
TopVNC crashes or is killed.

The Server tab and the WEB tab can serve at the same time. They share one
desktop capture, and each tab has its own listener: the Server tab's (for
example TLS on All networks) for native viewers, and the WEB tab's plain,
password-protected loopback listener for the gateway. Whichever tab starts
first starts the server; the other adds its listener, and stopping one tab
leaves the other serving. The display, size, mouse, and priority stay as the
first tab started them. If the Server tab listens on `rfb_target` itself, the
WEB tab shares that listener only when it is plain RFB with the same password;
otherwise it asks you to give the Server tab another port. See
[`specs/019-server-and-web-together/spec.md`](../specs/019-server-and-web-together/spec.md).
Before starting anything, the tab checks
`gateway.local.toml` with the gateway's own rules, the token file, the host
key, the gateway program, and cloudflared, and names the setup step that
fixes the first problem it finds.

**Verified 2026-11-04, locally only:** the start and stop order, time limits,
failures, server reuse rules, pairing, and output handling with simulated
programs; the gateway's supervised mode with the real gateway binary; and
child shutdown when the parent process is killed. The WEB tab has not yet
carried a live browser session.

### From three consoles

Open three console windows at the repository root and watch them yourself.
Start them in this order:

| Window | Command | Ready when it prints |
| --- | --- | --- |
| 1. Server | `cargo run --release -- --serve 127.0.0.1:5900 --unencrypted` | asks `VNC server password:`, then prints `Serving … on 127.0.0.1:5900.` |
| 2. Tunnel | `cargo web-tunnel` | `Registered tunnel connection` |
| 3. Gateway | `cargo web-gateway` | `Gateway listening on 127.0.0.1:6080 for the RFB server at 127.0.0.1:5900` |

`--unencrypted` is required: native hosting otherwise demands TLS
certificates, which the browser client does not speak, and stops with
`TLS hosting needs --tls-cert CERT.pem and --tls-key KEY.pem`. It is safe
here only because the server stays on loopback and the browser's traffic
reaches it through the gateway's end-to-end encrypted channel. Never combine
it with `0.0.0.0` or another address, and never add `--allow-insecure`: the
browser client refuses a server without a password.

You can also start the server from the **Server** tab instead of window 1:
launch TopVNC with `cargo run --release -- --unencrypted`, keep the tab on
`127.0.0.1:5900` (do not choose **All networks**), and set a password.

**Verified:** windows 1 and 3 ran live on 2026-10-31, before native TLS
became the default, so window 1 was then plain `--serve`; the `--unencrypted`
form has not yet carried a live browser session. Window 2 was the Phase 0
`cargo tunnel` with the same cloudflared arguments. On 2026-11-01, locally:
plain `--serve` stops with the TLS error above, and `cargo web-gateway`'s
program printed the ready line with a scratch config.

On the phone, open the app URL, sign in if asked, press **Connect**, and
enter the VNC password. Disconnect from the ☰ menu. When you are done, press
Ctrl+C in all three windows.

## Quick Tunnel (temporary, no account)

**Partially validated live (2026-11-06).** Windows Chromium paired, signed in
with a VNC password, and displayed a moving synthetic test pattern through a
real `trycloudflare.com` URL for ten minutes. Reconnect, restart with a new
URL/key, rejection of the old pairing, and fresh pairing also passed.
This does not validate iPhone Safari, macOS, or the real desktop flow from
the WEB tab. See the recorded checks and remaining limits in [Quick Tunnel validation](../specs/019-quick-tunnel-mode/validation.md).

For a temporary session, the **WEB** tab can use a Cloudflare
[Quick Tunnel](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/)
instead of the route above. It needs no Cloudflare account, Worker, Access
application, tunnel token, VPC service, `gateway.local.toml`, or host key file.
It needs only:

- `cloudflared` on `PATH`, or `CLOUDFLARED` set to its full path;
- the gateway built next to TopVNC: `cargo build --release -p topvnc-web-gateway`;
- the web app built: `npm run build` in `web/`. This needs the WebAssembly
  target and the pinned `wasm-bindgen-cli` from
  [step 1](#1-install-the-pinned-build-tools), but nothing from Cloudflare.

Use it:

1. On the **WEB** tab, choose **QUICK TUNNEL** and read the warning.
2. Type a VNC password and press **Start browser access**. TopVNC serves this
   desktop on `127.0.0.1:5900` as on the Access route, then starts the gateway
   and waits until it owns `127.0.0.1:6080`. Only then does it run
   `cloudflared tunnel --no-autoupdate --config <per-run-config> --url http://127.0.0.1:6080`.
   This private configuration pins ingress to the bound gateway; existing
   cloudflared configuration and inherited `TUNNEL_*` options are not used.
   After cloudflared prints its `https://<random>.trycloudflare.com` URL and
   registers a connection, TopVNC sends the exact origin to the gateway over
   stdin. The same listener stays held throughout startup. The
   gateway serves the web app itself, with the same Content-Security-Policy
   as the Worker, next to `/ws`. The panel shows the temporary app URL.
3. Press **Pair a browser** and scan the QR code with the phone camera, or use
   **Copy pairing URL** to open the invite directly in your browser. It
   opens the temporary URL with the invite. Check that the phone shows the
   same key as the TopVNC window, press **Connect**, and enter the VNC password.
4. **Stop browser access**, or closing TopVNC, ends the server, the tunnel,
   and the URL.

An occupied gateway port prevents cloudflared from starting. Gateway or
tunnel failure automatically ends this run. Stop and shutdown end public
forwarding before releasing the gateway listener. The generated configuration
contains no credentials and is removed after the connector stops. Returning
from a native viewer session preserves the running route in the WEB tab.

How it differs from the Access route:

| | Access route | Quick Tunnel |
| --- | --- | --- |
| Setup | About 30 minutes, with a Cloudflare account | Build only; no account |
| Address | Fixed `workers.dev` URL | A new random `trycloudflare.com` URL on every start |
| Who can open the page | Only the owner, after the Access sign-in | Anyone with the URL |
| What protects the desktop | Access, pairing, and the VNC password | Pairing and the VNC password only |
| Pairing | Once per browser | After every start: new URL, new host key |
| Enrolled browsers | Saved in `browsers.txt` | Kept in the gateway's memory for this run only |
| Page served by | The Worker | The gateway on the host, through the tunnel |
| Session limit | `max_session_seconds` and the Access sign-in expiry | 7200 s |
| Service | Cloudflare's free plans | Cloudflare's testing service, with no uptime guarantee |

Limitations:

- **A new URL every start.** The phone cannot find it again unless it can see
  the host's screen. Browser storage is per address, so every start also
  loses the phone's pairing.
- **Anyone with the URL can load the page and try to connect.** Only a browser
  paired in this run, which then also passes the VNC password, gets in.
  Strangers can, however, hold the gateway's eight pending-handshake slots for
  ten seconds each and delay your own connection.
- **For testing, per Cloudflare.** Quick Tunnels have no uptime guarantee, at
  most 200 requests in flight, and no Server-Sent Events. Cloudflare can
  throttle or end them at any time. Their docs do not cover WebSocket
  support, header forwarding, rate limits on creating tunnels, or traffic
  terms. A long, high-bitrate session is the main risk.
- **Often blocked.** The `trycloudflare.com` domain is widely abused, so some
  networks and security tools block it or flag cloudflared.
- **`--allowed-mail` is not used.** Cloudflare's optional email one-time PIN
  for Quick Tunnels is not known to cover WebSocket upgrades, so TopVNC does
  not rely on it.
- **Same trust limit.** TLS still ends at Cloudflare. The page now comes from
  the host through the tunnel, so no Worker can replace it, but Cloudflare's
  edge still could. See [Security model](#security-model).

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
| The WEB tab says `topvnc-web-gateway is not built next to TopVNC` | Run `cargo build --release -p topvnc-web-gateway` (or without `--release` if you run a debug TopVNC). |
| The WEB tab names a setup step | Fix that step of [One-time setup](#one-time-setup), then press **Start browser access** again. |
| A WEB tab row shows **Failed** | Click the row to see the program's last output. Nothing restarts by itself; press **Stop browser access**, fix the cause, and start again. |
| `cargo web-tunnel` says the token file is not found | Save the token in `gateway/cloudflare/tunnel-token.txt` ([step 5](#5-create-the-private-tunnel)). |
| `cargo web-tunnel` says `cloudflared` is not found | Install cloudflared, or set `CLOUDFLARED` to its full path. |
| Window 1 stops with `TLS hosting needs --tls-cert CERT.pem and --tls-key KEY.pem` | Add `--unencrypted` and keep the address `127.0.0.1:5900` ([Each session](#each-session)). |
| The gateway stops with `app origin must be an exact HTTPS workers.dev origin` | `app_origin` in `gateway.local.toml` still holds a placeholder, has uppercase letters, or ends with `/`. |
| `cargo run` says it `could not determine which binary to run` | Add `--bin topvnc-web-gateway`, as in [step 7](#7-create-the-host-key). |
| Connect fails right after a deploy or a long idle period | Possibly the [cold-start CPU risk](#free-plan-worker-cpu). Press **Connect** again, and look for `exceededCpu` in the Worker's analytics. |
| A build asks you to install a pinned tool | Run the `cargo install` command it prints, from the repository root ([step 1](#1-install-the-pinned-build-tools)). |
| The QR code does not scan | Maximize the console and use a dark background. |

The gateway console logs why each session ended (`relay closed: code=…` or
`session rejected: …`). An abrupt disconnect, such as the phone locking, can
appear as `1011` there.

## Free-plan Worker CPU

This is an **open risk, not a passed check.** Workers Free allows 10 ms of
CPU per invocation. Only `/ws` upgrades run Worker code; the page's files are
served without it. No paid plan is assumed.

**Rust Worker, 2026-10-30 to 2026-10-31.** Workers analytics counted 88
upgrades with no errors and none marked `exceededCpu`. Warm upgrades used
about 1.6-4 ms. The 7 cold upgrades, where a fresh isolate fetches and parses
the Access signing keys, used 13.0-18.7 ms; the highest was the first upgrade
after a deploy. None was terminated, but Cloudflare does not promise that it
will not terminate them. If it does, that connection attempt fails.

**Why, measured locally on 2026-11-01** (Node 22.18.0 on the Ryzen 9 9950X3D
host, which is faster than Cloudflare's servers). The same key parse and
RS256 check as the Worker, compiled to WebAssembly, cost about 7.3 ms in a
fresh process: 3.3 ms to load and instantiate the module, 1.1 ms for the
first key parse, and 3.0 ms for the first signature check, most of it
compiling and warming the WebAssembly code. Warm, the check took 0.44 ms.

**JavaScript Worker, since 2026-11-01.** The Worker was rewritten in plain
JavaScript using WebCrypto, which runs in the runtime's native code. The
upload shrank from 649.51 KiB to 14.01 KiB. The same fresh-process
measurement is about 2.8 ms: 1.3 ms to import the module (mostly Node's
module loader), 0.6 ms for the key parse, and 0.9 ms for the signature
check. Deployed on 2026-11-01 (version `d2962aca`), its first three
upgrades, including the first token check in a fresh isolate, used 0.59 ms,
5.87 ms and 4.39 ms, with no errors. That is a small sample from one desktop
browser, so the risk stays open until the iPhone run and the two-hour soak
add more.

The JavaScript checks mirror `crates/topvnc-access`, and the private gateway
still repeats every check with that crate before it opens a session.
`gateway/cloudflare/worker/test/access-vectors.json` holds tokens signed by
the crate's fixtures; the crate's tests keep the file current, and the
Worker's tests must accept and reject the same tokens.

## Runbook verification

On 2026-11-01 each step was checked without changing the Cloudflare account:
local builds and tests, Wrangler dry runs, read-only listings, CLI help, and
the commands' local behavior. The live evidence is the Phase 0 setup
(2026-10-30) and the production client switch (2026-10-31), which used the
probe's Worker, tunnel, and VPC service names (`topvnc-connection-spike`,
`topvnc-spike`), not `topvnc`.

| Step | Checked 2026-11-01 | Live evidence | Still needs a live run |
| --- | --- | --- | --- |
| 1. Tools | Version matches `Cargo.lock`; `npm ci` in both directories | Used for the live deploys | |
| 2. Sign in | `wrangler whoami` (4.155.0) | Free plans confirmed in the dashboard, 2026-10-30 | |
| 3. Config files | Keys match what the Worker and gateway read; dry runs with both configs | Owner's files deployed, 2026-10-31 | |
| 4. Access | Unauthenticated `/` and `/ws` get 302 to Access | Application saved, 2026-10-30 | Sign in with a different email and confirm Access refuses it |
| 5. Tunnel | `cargo web-tunnel` stops cleanly without the token file; cloudflared 2026.10.3 has `--token-file` | Phase 0 tunnel and `cargo tunnel` | Token at `gateway/cloudflare/tunnel-token.txt` and `cargo web-tunnel` reaching `Registered tunnel connection` |
| 6. VPC service | Flags match `vpc service create --help`; `vpc service list` shows the live service | Created as `topvnc-spike`, 2026-10-30 | |
| 7. Host key | Created, refused to overwrite, and restricted in a scratch directory; `--bin` fix | Phase 0 key `a1d1-…`, made by the probe's CLI | |
| 8. Build and deploy | Everything up to and including `deploy:check`; JavaScript Worker dry run | `npm run deploy`, 2026-10-31 (Rust Worker) | Deploy the current branch, which has changed since, and read cold-upgrade CPU |
| Pair your phone | Pairing through **Connect** in the local harness (2026-10-31) | Phase 0 `cargo pair` | `cargo web-pair`, then pair a browser over the live route |
| Each session | Plain `--serve` fails without TLS; gateway ready line | Desktop control from iPhone Safari, 2026-10-31, with plain `--serve` | One session with `--serve 127.0.0.1:5900 --unencrypted`, `cargo web-tunnel`, and `cargo web-gateway` |

The owner's remaining live run, from the repository root in PowerShell:

```powershell
# Step 5: move the Phase 0 connector token to the production path.
Move-Item gateway/cloudflare/spike/tunnel-token.txt gateway/cloudflare/tunnel-token.txt
icacls gateway/cloudflare/tunnel-token.txt /inheritance:r /grant:r "$($env:USERNAME):F" "SYSTEM:F"

# Step 8: deploy the current branch.
cd gateway/cloudflare/worker
npm run deploy
cd ../../..

# Each session, one console window each.
cargo run --release -- --serve 127.0.0.1:5900 --unencrypted
cargo web-tunnel
cargo web-pair    # or cargo web-gateway for an already paired browser
```

Then, on the phone, sign in, pair if needed, and connect. Also try a
different email at the Access sign-in, and return to Safari from the
background. Afterwards, read the `/ws` CPU times in Workers analytics.

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
  enter the password again. (Seen with the Phase 0 probe on iOS 27.0.1; not
  yet confirmed with the production client.)
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
- **Quick Tunnel mode** ([details](#quick-tunnel-temporary-no-account)) has no
  Access. It is explicit and opt-in, and is never a fallback. The gateway
  then requires no JWT. It accepts upgrades only from the exact
  `https://<label>.trycloudflare.com` origin cloudflared printed, which it
  receives at start from the WEB tab, never from a browser. A `workers.dev`
  origin is refused in this mode, and a `trycloudflare.com` origin on the
  Access route. Its host key is new for each run and held only in memory.
  Only browsers enrolled in that run are accepted, and the VNC password is
  still required. End-to-end encryption, fixed destinations, and the bounds
  above are unchanged.

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
| Live route, desktop control of a Windows host | iPhone Safari | Owner-reported (2026-10-31): display, pointer, tap clicks, two-finger scroll, pinch zoom, on-screen typing, toolbar keys, disconnect and reconnect. Background return not confirmed; iOS version not recorded. Host ran plain `--serve`, before native TLS became the default; the page deployed then predates the 2026-10-31 restyle |
| Worker CPU within the Free plan's 10 ms | Workers analytics | **Not passed; open risk.** Warm upgrades about 1.6-4 ms; 7 of 88 upgrades, the cold ones, used 13.0-18.7 ms. None was terminated. See [Free-plan Worker CPU](#free-plan-worker-cpu) |
| WEB tab start, stop, and pairing | Windows 11 | Locally tested with simulated programs and the real gateway binary (2026-11-04); live route not run |
| Quick Tunnel route | Windows 11 | Local WEB-tab simulations and real gateway tests; Windows Chromium paired and displayed a synthetic VNC test pattern over a real Quick Tunnel (2026-11-06). Captured cloudflared output is a regression fixture. Phone, real-desktop WEB-tab, macOS, and long-session checks remain pending; see [validation](../specs/019-quick-tunnel-mode/validation.md) |
| This runbook, end to end on a fresh account | Any | Not run. Steps checked locally and by dry run on 2026-11-01; see [Runbook verification](#runbook-verification) |
| Two-hour soak, mobile link latency | iPhone Safari | Not tested |
| macOS or Linux host | Any | Not tested |
