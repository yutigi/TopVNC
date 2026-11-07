# Feature Specification: Quick Tunnel mode for browser access

**Feature Branch**: `feat/quick-tunnel-mode` (git worktree `../TopVNC-quick-tunnel`, as the owner asked)

**Created**: 2026-11-05

**Status**: Plan approved by the owner on 2026-11-05. Implemented and tested
with simulated programs and local gateway tests. A Windows Chromium session
through a real Quick Tunnel displayed a synthetic VNC test pattern on
2026-11-06. Phone, real-desktop WEB-tab, macOS, and long-session checks remain
pending; see [validation.md](validation.md).

**Input**: Owner: "I have an idea": let any TopVNC user reach their host from a
phone browser through a free `https://<random>.trycloudflare.com` URL, with no
Cloudflare account and no setup.

## Scope

Browser access (features 014, 015, 018) needs a Cloudflare account and about
30 minutes of one-time setup: a Worker, a Zero Trust Access application, a
dashboard tunnel and its token, and a Workers VPC service
(`docs/cloudflare-gateway.md`). This feature adds an explicit, opt-in
**Quick Tunnel** route to the WEB tab. It needs only `cloudflared`, the built
gateway, and the built `web/dist`:

- `cloudflared tunnel --no-autoupdate --url http://127.0.0.1:6080` creates a
  temporary public URL with no account;
- the gateway serves the web app itself, next to `/ws`, because there is no
  Worker;
- there is no Cloudflare Access in front, so only end-to-end pairing (Noise,
  enrolled browser keys only) and the VNC password protect the desktop.

`specs/015-cloudflare-web-client/plan.md` ruled out Quick Tunnels as the
*permanent* route. That still holds: the Access route stays the default and is
unchanged. Quick Tunnel is a temporary, start-now-use-until-stop mode.

Decisions agreed with the owner on 2026-11-05:

1. The mode is explicit and opt-in, labeled "temporary URL, no Cloudflare
   sign-in; protected by pairing and the VNC password". AGENTS.md forbids a
   silent downgrade to an unauthenticated connection.
2. Pairing and the VNC password are mandatory. There is no plaintext fallback.
3. The gateway learns the page origin once, from cloudflared's output via the
   WEB tab, never from a browser.
4. `--allowed-mail` (Cloudflare's optional email one-time PIN) is not used or
   relied on; it is unverified for WebSocket upgrades.
5. Nothing is described as supported until it has run live.

Design choices made while implementing (within the approved plan):

- **Bind before publishing.** The WEB tab starts the gateway first, waits
  for `bound <address>`, then starts cloudflared with a generated configuration
  whose only ingress is that listener. Existing cloudflared configurations
  and inherited `TUNNEL_*` options cannot change this route. After learning
  the URL and a registered connection, the tab sends `origin <url>` on stdin.
  The gateway retains the same socket until shutdown and admits no HTTP
  before the origin is set. A second origin command ends the gateway.
- **Stop forwarding first.** Gateway or tunnel failure ends the Quick Tunnel
  run; forwarding stops before the gateway listener is released on shutdown.
- **Each run gets a new host identity.** Browser pairing storage is per
  origin, and every start gives a new origin, so every run needs a new pairing
  anyway. The gateway creates a fresh host key in memory and keeps enrolled
  browser keys in memory only. No identity is written to disk, the eight-key
  enrollment list never fills with dead keys, and a browser key enrolled on the
  Access route is never accepted without Access.

Out of scope: a stable URL or any rendezvous for the phone to find a new URL;
`--allowed-mail`; a named tunnel without Access; changing the Access route;
packaging without a repository checkout.

## Limitations

From Cloudflare's Quick Tunnel documentation
(https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/, checked
2026-11-05) and this design.

| Limitation | Effect on TopVNC |
| --- | --- |
| The hostname changes on every start | The phone cannot find the new URL after a restart unless it can see the host's screen. There is no rendezvous point without an account. |
| Browser storage is per origin | Every new URL loses the browser's pairing. The user pairs again from the host's screen after each start. |
| "For testing and development", no uptime guarantee | It is not the default route. Cloudflare can throttle or end it at any time. |
| Anyone with the URL can reach it | There is no Access lock. Strangers can load the page and attempt handshakes; pairing and the VNC password still apply. They can also hold the eight pending-handshake slots for ten seconds each, delaying the owner (denial of service, not access). |
| Optional `--allowed-mail` | Not used. Unknown whether it covers WebSocket upgrades. |
| 200 in-flight requests per tunnel; no SSE | Fine: one WebSocket plus five page files. |
| The URL ends when `cloudflared` stops | The WEB tab controls cloudflared's lifetime, so this is expected. |
| Not in the docs: WebSocket support, header forwarding, creation rate limits, traffic terms | Must be checked live. A long, high-bitrate stream over a testing service is the main risk. |
| `*.trycloudflare.com` is often abused | Some networks and security tools block the domain or flag cloudflared. |
| Latency | The same Cloudflare hop as the named tunnel. |
| Trust | TLS ends at Cloudflare, which carries the page and the ciphertext. The page now comes from the host through the tunnel, so no Worker can replace it, but Cloudflare's edge still could. Pairing pins the host key, so a substituted page is the residual risk, as on the Access route. |

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Start a temporary browser route with no account (Priority: P1)

A user without any Cloudflare setup opens TopVNC, goes to the WEB tab, selects
**QUICK TUNNEL**, reads the warning, types a VNC password, and presses Start.
The rows go Ready, the panel shows the temporary app URL, and **Pair a
browser** shows a QR code. One scan from the phone opens the URL and pairs.
After entering the VNC password the phone shows the desktop.

**Independent Test**: with simulated child programs, run the start, pair, and
stop flow and check the arguments of each child. Locally, run the gateway in
Quick Tunnel mode and fetch the page and complete a paired session.

**Acceptance Scenarios**:

1. **Given** the WEB tab is idle, **When** the user selects Quick Tunnel,
   **Then** the panel states, before Start: temporary URL, no Cloudflare
   sign-in, protected by pairing and the VNC password, anyone with the URL can
   open the page, each start gives a new URL and needs a new pairing, and
   phone and long-session checks remain pending.
2. **Given** Quick Tunnel is selected, **When** Start is pressed, **Then**
   TopVNC needs no `gateway.local.toml`, token file, or host key file. It checks
   only for `cloudflared`, the gateway program, and the built `web/dist`, and
   names whichever is missing.
3. **Given** Start, **Then** TopVNC serves on `127.0.0.1:5900` (loopback, plain
   RFB, password required), starts the gateway and waits for it to bind
   `127.0.0.1:6080`, then runs
   cloudflared with an explicit per-run configuration pinned to that listener,
   and marks the tunnel Ready only after cloudflared has printed a valid
   `https://<label>.trycloudflare.com` URL **and** registered a connection.
4. **Given** the tunnel is Ready, **Then** the already-bound gateway receives
   that exact origin over stdin and serves the page and `/ws` on
   `127.0.0.1:6080`.
5. **Given** the gateway is Ready, **When** the user pairs, **Then** the QR
   code encodes the temporary URL with the invite in the fragment.
6. **Given** a running Quick Tunnel, **When** the user stops and starts again,
   **Then** a new URL and a new host key fingerprint are shown, and the old
   pairing no longer works.
7. An occupied gateway port starts no connector. A gateway failure stops
   forwarding immediately; graceful shutdown stops the connector before
   closing the gateway. Returning from a native viewer restores the active
   route, its URL, and its route-specific security explanation.

### User Story 2 - The route stays as safe as pairing allows (Priority: P1)

**Acceptance Scenarios**:

1. A WebSocket upgrade is accepted only from the exact learned origin, on
   `GET /ws` with no query, with the `binary` subprotocol.
2. In Quick Tunnel mode the gateway rejects a `workers.dev` origin; in Access
   mode it rejects a `trycloudflare.com` origin.
3. In Quick Tunnel mode a browser key that is not enrolled in this run is
   rejected after the handshake, and no TCP connection to the desktop opens.
4. A session lasts at most `max_session_seconds` (7200).
5. The gateway serves only the five fixed page files, with fixed content
   types and the same strict Content-Security-Policy and security headers as
   `web/_headers`. Any other path is 404.

### Edge Cases

- cloudflared cannot create the tunnel (rate limit, blocked domain, network):
  it exits or never prints a URL, and the Tunnel row fails with its output.
- cloudflared prints other `https://` links (terms of service, docs): only a
  single-label `trycloudflare.com` origin is accepted.
- `web/dist` is not built: Start refuses and says to run `npm run build` in
  `web/`.
- A page file grows beyond the gateway's size limit: the gateway refuses to
  start.
- The connector reconnects: the URL stays the same; the row shows
  RECONNECTING as on the Access route.

## Requirements *(mandatory)*

- **FR-001**: The WEB tab MUST offer the Access route (default) and the Quick
  Tunnel route as an explicit choice, changeable only while stopped.
- **FR-002**: The gateway MUST have two admission modes: Access (Access JWT
  for the owner, a `workers.dev` origin, today's behavior) and Pairing only (no
  JWT, a `trycloudflare.com` origin). The mode decides which origin rule
  applies.
- **FR-003**: The Quick Tunnel origin validator MUST accept only
  `https://<label>.trycloudflare.com` with one lowercase DNS label (letters,
  digits, inner hyphens, at most 63 characters) and no port, path, query,
  fragment, or user information.
- **FR-004**: In Pairing-only mode the gateway MUST use an in-memory host key
  and in-memory enrollments, MUST serve the built page itself, and MUST cap
  sessions at `max_session_seconds`.
- **FR-005**: The pairing link MUST stay secret as before: shown only as a QR
  code, never as text, in output, or in logs.
- **FR-006**: The docs MUST describe the route, its limitations, and that it
  still needs phone and long-session validation.

## Success Criteria

- **SC-001**: `cargo fmt --all`, `cargo test --workspace`,
  `cargo clippy --workspace --all-targets`, and `npm test` in `web/` pass.
- **SC-002**: Tests cover the origin validator, both admission modes'
  origin rules, Pairing-only rejection of unenrolled keys, page serving and
  headers, parsing of real Quick Tunnel output including its banner box, and
  the WEB tab start and stop flow with simulated programs.

## Validation

Results are recorded in [validation.md](validation.md). The live Chromium
check uses a synthetic password-protected VNC server; the owner still needs
to check the real desktop and phone flow:

1. Start Quick Tunnel from the WEB tab on Windows; scan from iPhone Safari;
   pair, enter the VNC password, and use the desktop.
2. Confirm cloudflared forwards the WebSocket upgrade and the `Origin` header
   unchanged.
3. Stop and start again; confirm the old URL fails cleanly and re-pairing
   works.
4. Run a long session and watch for throttling or disconnects.
