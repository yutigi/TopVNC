# Feature Specification: Cloudflare web access from the TopVNC window

**Feature Branch**: `feat/cloudflare-web-client` (owner works on this branch in the main checkout)

**Created**: 2026-11-04

**Status**: Approved by the owner on 2026-11-04. Phase 1 (Run view) implemented and locally tested; live check and Phase 2 (Setup) pending

**Input**: Owner: "May I have the UI for help me setup the cloudflare tunnel and gateway?"

## Scope

Browser access through Cloudflare (features 014 and 015) works, but it needs a
30-minute manual runbook (`docs/cloudflare-gateway.md`, steps 1-8) once, and
three console windows every session. This feature adds a **WEB** tab to the
native TopVNC window with two views:

- **Run** (P1): start, stop, and watch the desktop server, the tunnel
  connector, and the web gateway with one button, and pair a browser from a QR
  code drawn in the window. TopVNC supervises the connector and gateway as
  child processes.
- **Setup** (P2): a guided, checkable walk through the one-time setup, written
  for a new user with their own Cloudflare account, not only for the owner.

Decisions agreed with the owner on 2026-11-04:

1. Both views are in this one spec; Run is built first.
2. TopVNC launches and supervises the tunnel connector and the gateway as
   child processes. They never outlive TopVNC.
3. The Zero Trust dashboard steps (Access application, Tunnel) stay manual.
   Setup links to the right page, explains what to click, and validates the
   values the user brings back. No Cloudflare API token is requested.
4. Setup is for other people too: one owner, one host, one Worker per person
   (`OWNER_EMAIL` stays a single address).

Out of scope: Cloudflare API automation of Access or Tunnel; a packaged
installer that works without a repository checkout; more than one host or
owner; a UI to list or revoke paired browsers (revocation stays the manual
file edit in the runbook); paid Cloudflare plans.

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Run browser access without consoles (Priority: P1)

The owner opens TopVNC, goes to WEB → Run, types the VNC password, and presses
**Start**. Three status rows (Server, Tunnel, Gateway) go from Stopped to
Starting to Ready. The owner then connects from the phone. **Stop**, or closing
TopVNC, ends all three.

**Why this priority**: it removes the three consoles from every session, which
is the daily cost. It builds on the Server tab's existing in-process hosting.

**Independent Test**: with an existing, valid `gateway.local.toml`, token file,
and host key, start and stop from the Run view and check the status rows, the
process list, and the browser route. With fake child programs, test each
failure path automatically.

**Acceptance Scenarios**:

1. **Given** valid local configuration and no server running, **When** the
   owner enters a password and presses Start, **Then** TopVNC serves on the
   gateway's `rfb_target` (loopback, plain RFB, password required), then starts
   the connector, then the gateway, and each row turns Ready only on its real
   ready signal (`Serving … on 127.0.0.1:5900`, `Registered tunnel connection`,
   `Gateway listening on …`).
2. **Given** all three are Ready, **When** the owner presses Stop or closes
   TopVNC, **Then** the gateway, connector, and server stop in that order and
   no `topvnc-web-gateway` or `cloudflared` process is left running.
3. **Given** TopVNC is killed (Task Manager, crash), **Then** its child
   processes end too.
4. **Given** the Server tab is already serving, **When** Start is pressed,
   **Then** TopVNC reuses that server only if it is on the gateway's loopback
   `rfb_target`, password-protected, and plain RFB. Otherwise it refuses with an
   explanation and changes nothing. *Superseded by feature 019: the WEB tab now
   adds its own listener to the running server, so both tabs serve at once.*
5. **Given** a child exits or never reaches Ready within its time limit,
   **Then** its row shows Failed with the last lines of its output, and the
   other rows stay as they are. No automatic restart loop.
6. **Given** the connector is missing (`cloudflared` not found), the token file
   is missing, or the configuration is invalid, **When** Start is pressed,
   **Then** TopVNC names the problem and the Setup step that fixes it, and
   starts nothing.

---

### User Story 2 - Pair a browser from the TopVNC window (Priority: P1)

The owner presses **Pair a browser** in the Run view. TopVNC shows the host key
fingerprint, a QR code, and a ten-minute countdown in its own window. After the
phone enrolls, the QR code disappears and the view says a browser was enrolled.

**Why this priority**: pairing is required before first use, and today it needs
a maximized console to keep the QR code from wrapping.

**Independent Test**: pair a browser with the local test harness
(`local_web` example) through the Run view, then on the live route.

**Acceptance Scenarios**:

1. **Given** the gateway is running or stopped, **When** the owner presses Pair
   a browser, **Then** browser access starts if needed, the running gateway
   opens a one-time invite without restarting (a connected phone stays
   connected), and TopVNC draws the QR code from the invite, with the
   fingerprint and the reminder to sign in to the web app before scanning.
2. **Given** a QR code is shown, **When** a browser enrolls, the ten minutes
   pass, or the owner presses Cancel, **Then** the QR code is removed from the
   window.
3. The pairing link is never shown as text, copied to the clipboard, written
   to a file or log, or kept after the QR code is removed.

---

### User Story 3 - Guided first-time setup (Priority: P2)

A new user with a Cloudflare account and a TopVNC repository checkout opens
WEB → Setup. A checklist of the eight runbook steps shows which are already
done, based on what TopVNC can check locally. Each step explains what to do,
offers a button for the parts TopVNC can do, and validates the values the user
types or pastes. At the end the Run view is ready to use.

**Why this priority**: it is needed once per person, but it is the main barrier
for anyone other than the owner.

**Independent Test**: run Setup against a scratch directory with a fresh
Cloudflare account (or a mocked Wrangler for the automated part), and check
that the files it writes are the same as a correct manual setup.

**Acceptance Scenarios**:

1. **Tools**: Setup checks for the WebAssembly target, Node.js 22 and npm,
   `cloudflared` with `tunnel run --token-file`, the pinned `wasm-bindgen-cli`
   in `.tools`, and the Worker's `node_modules`. For a missing local tool it can
   run the runbook's pinned install command on request.
2. **Sign in**: a button runs `npm run login` (opens the browser) and then
   `wrangler whoami`, and offers the account ID it prints (with a choice when
   there are several). Setup reminds the user to keep Workers and Zero Trust on
   the Free plan, with links to check.
3. **Config files**: the user enters the Worker name and their workers.dev
   subdomain. Setup shows the app URL it will use and writes
   `wrangler.local.toml` and `gateway.local.toml` from the example files, with
   the gateway's own validation rules. It never overwrites an existing file
   without showing what changes and asking.
4. **Access** (manual): Setup opens the Zero Trust dashboard, lists exactly
   what to create (self-hosted application for the exact app hostname, one
   Allow policy for one email, no Bypass, preview URLs off, not the Worker's own
   Access toggle), and validates the team URL, AUD tag, and email the user
   brings back before writing them to both files.
5. **Tunnel** (manual): Setup opens the dashboard and explains how to create a
   cloudflared tunnel for Workers VPC without a public hostname. The user
   pastes the connector token into a masked field. Setup writes it to
   `gateway/cloudflare/tunnel-token.txt`, restricts the file, clears the field,
   and from then on shows only "token saved". The tunnel ID comes from
   `wrangler tunnel list` (read-only) or is typed in.
6. **VPC service**: Setup shows the exact `wrangler vpc service create` command
   and runs it only after the user confirms. It can instead pick an existing
   service from `wrangler vpc service list`. The service ID goes into
   `wrangler.local.toml`.
7. **Host key**: Setup creates the host key, restricts the file, and shows the
   fingerprint. It refuses to replace an existing key; rotation stays manual.
8. **Build and deploy**: Setup runs the web build and `npm run deploy:check`,
   shows the results, and runs `npm run deploy` only after the user confirms.
   It then requests the app URL without signing in and marks the step done only
   if the answer is a redirect to the configured Access team. Otherwise it
   shows a stop warning (the runbook's "page opens without sign-in" case).

---

### Edge Cases

- Wrangler or cloudflared output changes format and a value cannot be read:
  Setup says so and lets the user type the value instead. It never guesses.
- A long command (install, build, deploy) hangs: it has a time limit and a
  Cancel button. Cancel ends the whole process tree.
- The user closes TopVNC during a Setup command: the command is ended like a
  Run child. A half-written config file is never left behind (write to a
  temporary file, then rename).
- Port 5900 or 6080 is already in use (for example by a console the user
  still runs): the row fails with "address in use" and names the port.
- The connector loses its connection after Ready: the Tunnel row leaves Ready
  when cloudflared reports the loss, and returns when it re-registers.
- `gateway.local.toml` points `rfb_target` at a non-loopback address or the
  config still holds placeholders: Start refuses before starting anything.
- Two TopVNC windows try to run browser access at once: the second one finds
  the ports in use and refuses.
- The repository checkout is not found (TopVNC run from elsewhere): Setup
  says it needs a repository checkout and where it looked. Run still works if
  the gateway binary and config paths can be found.

## Requirements *(mandatory)*

### Functional Requirements

**Run view**

- **FR-001**: The Run view MUST start the desktop server in-process (the
  existing Server-tab host) on the gateway's `rfb_target`, which MUST be
  loopback, with plain RFB and a required VNC password. It MUST NOT offer
  All networks, `--allow-insecure`, or an empty password.
- **FR-002**: The connector and the gateway MUST run as child processes
  without console windows, in the order server → connector → gateway, and stop
  in reverse order.
- **FR-003**: Child processes MUST NOT outlive TopVNC, including when TopVNC is
  killed. This includes cloudflared started by the connector wrapper.
- **FR-004**: Each status row MUST show Stopped, Starting, Ready, Stopping, or
  Failed, from real signals (the server's Serving notice, the ready lines,
  process exit), with a per-row start time limit.
- **FR-005**: TopVNC MUST keep only a bounded amount of each child's output in
  memory (a fixed number of recent lines), MUST NOT write it to disk, and MUST
  remove pairing links from it before keeping or showing it.
- **FR-006**: The Run view MUST check the configuration before starting
  anything (the same rules as `Gateway::new`, the presence of the token file
  and host key, and that cloudflared can be found) and report the first
  problem with the Setup step that fixes it.
- **FR-007**: Pairing MUST draw the QR code in the TopVNC window from the
  gateway's invite, with the fingerprint and a countdown, and MUST remove it on
  enrollment, expiry, or Cancel. The link MUST NOT be shown as text, copied,
  logged, or saved.
- **FR-008**: The VNC password MUST NOT be saved; it is used for this run only,
  like the Server tab's.

**Setup view**

- **FR-010**: Setup MUST show the eight steps with a done/not done state that
  is re-checked from local evidence (files present and valid, tools found,
  fingerprint readable), not remembered from earlier clicks.
- **FR-011**: Setup MUST validate each value before writing it (app origin
  rules shared with the gateway; issuer as an `https://<team>.cloudflareaccess.com`
  origin; a non-placeholder AUD tag and email; an account ID and service ID in
  the formats Wrangler prints) and MUST name the field and the rule that failed.
- **FR-012**: Setup MUST write config files atomically, MUST keep the example
  files' comments, and MUST NOT overwrite an existing file without showing the
  change and getting confirmation.
- **FR-013**: The tunnel token MUST be accepted only through a masked field,
  written straight to the token file, and never displayed, logged, copied to
  the clipboard, passed on a command line, or kept in memory after the write
  beyond what the write needs.
- **FR-014**: The token file and the host key MUST be restricted to the current
  user and SYSTEM on Windows (as the runbook's `icacls` does) and to the owner
  (mode 0600) elsewhere, and Setup MUST verify the restriction after writing.
- **FR-015**: Every action that changes the user's Cloudflare account
  (`vpc service create`, `deploy`) MUST show the exact command and run only
  after an explicit confirmation in Setup. Read-only commands (`whoami`,
  `tunnel list`, `vpc service list`, `deploy:check`) MAY run on a button press.
- **FR-016**: After deploy, Setup MUST check that the app URL redirects an
  unauthenticated request to the configured Access team, and MUST NOT mark
  setup complete otherwise.
- **FR-017**: Dashboard steps MUST stay manual: Setup opens the relevant
  Cloudflare documentation or dashboard page in the system browser and
  explains the exact settings. It MUST NOT ask for a Cloudflare API token.
- **FR-018**: Setup MUST NOT suggest or select a paid Cloudflare plan.

**Both**

- **FR-020**: TopVNC MUST NOT print or log the tunnel token, the host key, a
  pairing link, a JWT, or the VNC password. The owner's email is shown only in
  Setup's own fields.
- **FR-021**: Platform-specific process supervision and file permissions MUST
  sit behind a small module with a Windows implementation first. The WEB tab
  MUST say plainly where a platform is untested.
- **FR-022**: The runbook (`docs/cloudflare-gateway.md`) MUST describe the WEB
  tab as an alternative to the consoles and keep the manual steps, and MUST
  record what was verified live and what was not.

### Key Entities

- **Web access configuration**: the two local config files, the token file,
  and the host key, with a derived "ready to run" check and a per-step setup
  state.
- **Supervised process**: one child program with its state, start time limit,
  bounded recent output, and ready-line rule.
- **Pairing view**: the current invite's QR code, fingerprint, and expiry,
  held only while shown.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: With setup done, a session needs one Start press and the VNC
  password, and no console window.
- **SC-002**: Within 5 seconds of Stop, of closing TopVNC, or of killing it,
  no `topvnc-web-gateway`, `topvnc-tunnel`, or `cloudflared` process started by
  it remains.
- **SC-003**: Automated tests feed a sentinel token, a sentinel pairing link,
  and a sentinel password through Setup and Run, and none of them appears in
  any UI text other than the QR code, in kept output, in stderr, or in a file
  other than the token file.
- **SC-004**: Setup refuses to mark step 8 done when the app URL answers
  without an Access redirect (tested against a local fake).
- **SC-005**: A fresh setup through the WEB tab, run by the owner on a scratch
  Cloudflare Worker name, reaches a working phone session, and its config files
  match what the runbook would produce. Until this live run happens, the
  feature is described as locally tested only.

## Assumptions

- Windows is the host that will be tested. macOS and Linux get the same code
  paths where practical but are not called supported until tested there.
- Setup runs in a repository checkout with the runbook's prerequisites
  (Rust, Node.js 22, cloudflared, a Cloudflare account with 2FA). It does not
  install Rust, Node.js, or cloudflared itself.
- `topvnc-web-gateway` and `topvnc-tunnel` are built next to the `topvnc`
  binary (`cargo build --release` builds all three). TopVNC looks for them
  there.
- The gateway and Worker keep their current configuration format. Changes the
  gateway needs for supervision (for example a machine-readable pairing or
  ready line, or restricting the host key file itself on Windows) are part of
  this feature and keep the console commands working.
- The trust limit accepted on 2026-10-30 (Cloudflare serves the page code) is
  unchanged by this feature.
