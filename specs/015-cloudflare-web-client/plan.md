# Implementation Plan: Personal browser client on workers.dev (free tier)

**Branch**: `feat/cloudflare-web-client` (created from `main` at `03d1881`) | **Date**: 2026-10-31 | **Status**: Live encrypted echo passed in iPhone Safari and Edge; production client, gateway, and Worker implemented and locally validated; live desktop control, device, and resource gates open

## Reconciliation with current main (2026-10-30)

The original worktree was removed during an authorized cleanup in another
session; its uncommitted implementation was preserved in a Git stash. Restored
that stash, without dropping it, into this new worktree branched from current
`main`. Feature 013 now belongs to multi-monitor; this browser feature is 015.
No changes were made to the main checkout or feature 014.

Main's feature 014 records a mandatory E2E requirement before Cloudflare carries
RFB. This supersedes the earlier proposal below to allow readable RFB at the
edge. The restored Rust relay/JWT/noVNC harness is transport groundwork only:
it does not encrypt RFB against Cloudflare. The CLI now forces a synthetic
echo target, and the browser sends generated samples without any password
field. Do not deploy against a real desktop. Account and owner-only Access
setup can proceed independently.

The browser-to-actual-host encrypted transport is now specified below (T023),
implemented in the isolated harness, and tested locally. The owner accepted its
code-delivery trust limit. Live encrypted-route and iPhone validation remain
open, so the harness still carries only synthetic echo. SSH from the native plan
is not directly usable by Safari. Access JWTs are not payload encryption.
`--synthetic-echo` remains the only mode and cannot select an RFB target.

## Production implementation (2026-10-31)

Phases 1-4 are implemented and validated locally (validation.md). The
decisions that differ from, or narrow, the phases below:

- **Crates.** `topvnc-rfb` (sans-IO protocol), `topvnc-e2e` and
  `topvnc-access` (promoted from the spike's `e2e` and `policy`),
  `topvnc-web-gateway`, and `topvnc-web` (the WASM client) are root workspace
  members. The Worker, `gateway/cloudflare/worker`, is its own
  WebAssembly-only workspace. It serves `web/dist` and is the spike forwarder
  with production naming. The spike stays as the synthetic route probe.
- **Gateway start.** The gateway is a separate opt-in binary
  (`cargo web-gateway`) that relays to `topvnc --serve` on loopback. It is not
  a `--serve` setting, which keeps tokio, axum, and the JWT stack out of the
  native app. It reuses the Phase 0 Access application, VPC service, Tunnel,
  host identity, and enrollments.
- **Session slot.** The slot is claimed after the E2E handshake. A reconnect
  from the same enrolled browser replaces its own stale session, which fixes
  the 60-80 s busy window seen in T026. Another browser gets a readable 1013
  close instead of an invisible HTTP 409.
- **Presentation.** A dedicated module Worker owns the WebSocket, the WASM
  session, and an OffscreenCanvas 2D context, and draws only the accumulated
  damage with `putImageData` from a view of WebAssembly memory. Browsers
  without OffscreenCanvas run the same module on the page. WebGL2 is deferred
  until measurements show 2D drawing is a bottleneck.
- **Encodings and flow.** The client advertises Tight (quality chosen by the
  user, default 6), Zlib, Raw, DesktopSize, served size and display,
  ContinuousUpdates, Fence, relative pointer, and extended buttons. Fence
  replies leave after the preceding updates are applied, so the server's
  flow control sees the browser's real speed.
- **T026 usability findings, fixed in the production page.** A `#pair=`
  pasted into an open tab is read and stripped. A link for the already paired
  host does not replace the pairing. Gateway close reasons are shown, and an
  expired Access sign-in is told apart from an offline gateway. Abrupt
  disconnects are no longer logged as 1009.

Live deployment, desktop control through Cloudflare, and the device checks
are still open: T026's last check, T011, T017, T020, and T021's live steps.

## Browser-to-host E2E design (T023)

**Threat model and owner decision (2026-10-30).** Cloudflare must not receive
readable RFB credentials, frames, input, or clipboard data. Two limits remain:

- Cloudflare serves the browser's JS/WASM, so it, or whoever controls the
  owner's Cloudflare account or Worker deployment, could serve modified code
  that reads the session. No same-origin design can detect that.
- Serving the client from another origin would need the workers.dev Access
  cookie on a cross-site WebSocket, which Safari's third-party cookie blocking
  is expected to prevent (not tested). iOS offers no browser-side code pin.

The owner accepted this limit with disclosure. The channel protects against
passive observation at the edge, Worker/VPC/Tunnel logs or misconfiguration,
stolen Access JWTs or tunnel tokens, and host impersonation. It does not protect
against active code substitution. The page and README say so; no stronger
confidentiality claim is made.

**Protocol.** Noise `XK_25519_ChaChaPoly_BLAKE2s` (Noise rev. 34) via the
maintained `snow` crate's pure-Rust resolver, shared by the native gateway and
the browser WASM (`gateway/cloudflare/spike/e2e/`). No bespoke primitives or
handshake. XK authenticates the host before the browser sends anything
sensitive and hides the browser's static key from the relay. The browser's
identity is verified by the host only after a full round trip, so the gateway
opens its TCP target only after an authorized handshake. The production RFB
client must still perform VNC password authentication inside this channel.

**Endpoints.** The Rust host gateway on the actual host is the responder and
relays decrypted bytes to its loopback target. The Worker and the native relay
never choose destinations; the Worker forwards the upgrade unchanged.

**Bootstrap and identity.**

- `--init-host-key` creates the host's static X25519 key once (create-new; no
  silent replacement). Rotation is explicit: delete the key and re-pair.
- `--pair` prints a terminal QR code on the host containing
  `https://<origin>/#pair=v1.<host public key>.<256-bit one-time secret>`.
  The secret is held only in gateway memory, expires after ten minutes, and is
  consumed by the first successful pairing. The phone camera transfers it
  independently of Cloudflare; browsers do not send fragments in requests, and
  the page removes it from the address bar.
- Pairing uses Noise `XKpsk3` with that secret: the browser pins the host key,
  and the host learns the new browser key only with proof of the secret. The
  gateway appends the key to `browser_keys_file`, at most eight keys.
  Ordinary sessions use `XK` and require an enrolled key.
- The browser keeps the pinned host key and its private key in `localStorage`.
  Missing or altered state means "not paired". Safari's seven-day storage
  expiry therefore fails closed and requires pairing again. Revocation means
  removing the line on the host.

**Framing and bounds.** The first message has a two-byte version/mode header
bound into the Noise prologue, so changing the mode fails the handshake.
Handshake messages are fixed at 50, 48, and 64 bytes with empty payloads and a
ten-second deadline. After that, each binary WebSocket message is exactly one
Noise transport message carrying 1–16 KiB of plaintext plus a 16-byte tag. The
WebSocket message/fragment limit equals that maximum. Sequential implicit nonces
make loss, reordering, replay, truncation, and tampering authentication
failures; the channel is then poisoned.

**Failure behavior.** The gateway closes with a reason (1008 for handshake,
enrollment, or integrity failure; 1011 for an offline target) and never falls
back to plaintext. The browser shows host-verification failure without a
fallback, and offers re-pairing only for missing/unenrolled state or the
owner's explicit rotation.

**Validation.** Local Rust tests cover handshake and pairing success, a wrong
pinned host key, a substituted host reply, unenrolled keys, wrong, expired, and
consumed invites, mode/header tampering, fixed sizes, and transport replay,
reordering, loss, tampering, and size bounds. They also cover target
opening only after authorization, handshake timeout, and persistence. A Node
smoke test exercises the built WASM bindings. On the deployed route, the
bundled code interoperated with iPhone Safari and Edge. The live checks also
showed that the `#pair=` link can survive in browser tab or history state
despite being removed from the address bar (research.md). T026 is still open.

**Before desktop traffic.** T026 must pass on the deployed route. The
production client must then carry RFB, with VNC password authentication, only
through this channel. T011, the resource gates, and the US2 extraction remain
as listed in tasks.md.

## Owner decisions (2026-10-29)

| Area | Decision |
| --- | --- |
| Users | The owner's personal use first. Serving other users is a later phase. |
| Cost | Free Cloudflare services only; no Workers Paid or other paid dependency. |
| Main use | Desktop control from a mobile browser. Gaming is a later enhancement. |
| Host transport | An opt-in WebSocket listener inside TopVNC is acceptable. |
| Relay language | Rust for native and edge relay code; workers-rs for the proposed Worker. |
| Public address | No owned domain. Use Cloudflare's free workers.dev address; no custom-domain purchase is assumed. |
| Browser code trust (2026-10-30) | Accept, with disclosure, that Cloudflare-served client code is trusted not to be substituted; E2E still keeps the relay path to ciphertext. |

The owner authorized implementation on 2026-10-30. Begin the isolated Phase 0
spike and complete its live gates before production refactoring. A free
workers.dev address hosts the synthetic probe, and the basic authenticated
private route passed in Edge. Desktop traffic, resource limits, and Safari
remain gated. The owner requested a commit of this work on 2026-10-30.

## Summary

Goal: the owner opens a URL in iPhone Safari, signs in, and controls their own
host running `topvnc --serve`. Nothing is installed on the phone. Start with
one host and one active browser session.

**Free app hosting is documented; end-to-end host access is unverified.**
Capture and input injection still run on the host. Serve the browser app's
static HTML, JS loader, and Rust/WASM files at
`https://topvnc.<account-subdomain>.workers.dev`. The phone uses `/ws` on that
same origin. No purchased domain or phone-side VPN is required by this design.

The preferred **Phase 0 candidate** combines the accepted host WebSocket
listener (Option B) with a private Workers VPC binding. A small Rust Worker
authenticates and forwards the WebSocket upgrade through the binding's HTTP
`fetch()` API, aiming to return the upgraded response without handling each
message in Worker code. The native Rust listener handles RFB forwarding and
bounded queues. HTTP documentation alone did not establish WebSocket support;
the deployed synthetic probe has now passed upgrade/passthrough and loopback
routing in Edge. See research.md for the limited evidence and remaining gates.

**2026-11-01:** the production Worker is now plain JavaScript with WebCrypto
instead of Rust compiled to WebAssembly (T027). Cold upgrades of the Rust
Worker used 13.0-18.7 ms of CPU against the Free plan's 10 ms, and local
measurement put most of that in loading and warming the WebAssembly module.
The JavaScript checks mirror `crates/topvnc-access`, are tested against tokens
signed by that crate, and the gateway still repeats them in Rust.

```text
iPhone Safari
   │
   ▼
topvnc.<account-subdomain>.workers.dev
   │  hostname-based Access application (owner only)
   ├─ / and assets ── Workers Static Assets (HTML / JS / WASM)
   └─ /ws ── Worker (JWT + fixed destination + upgrade forwarding)
                 │ Workers VPC fetch() / WebSocket upgrade: TO VERIFY
                 ▼
             private Cloudflare Tunnel (no public Tunnel hostname)
                 │ encrypted, outbound from host
                 ▼
             cloudflared on host
                 │ local HTTP / WebSocket target: TO VERIFY
                 ▼
             Rust WebSocket listener (opt-in)
                 │ fixed loopback TCP target
                 ▼
             TopVNC RFB server (VNC password auth)
```

Protect the exact workers.dev hostname with a **hostname-based Access
application**. Current Cloudflare documentation says Worker-level Access
policies reject WebSocket upgrades with HTTP 403 and directs WebSocket users
to hostname-based protection. Disable unused preview/version entry points or
protect them equivalently. Authenticate in a normal browser navigation before
opening `/ws`; an upgrade cannot perform an interactive login.
[Workers Access configuration and WebSocket limitation](https://developers.cloudflare.com/workers/configuration/cloudflare-access/).
Safari sign-in, expiry, and reauthentication are Phase 0 gates.

**Wire format.** Each `binary` WebSocket message carries one Noise handshake
or transport message (see the E2E design above); the probe's plaintext is
generated echo samples only. Production RFB must travel inside that channel.
The gateway terminates E2E and relays bytes but never parses RFB or accepts a
destination from the browser. Message boundaries have no RFB meaning.

## Free-tier feasibility (documentation checked 2026-10-29)

| Component | Evidence and planning consequence |
| --- | --- |
| Free public address | A Worker can use its Cloudflare-provided workers.dev hostname, suitable for personal/hobby use. No custom domain is needed for this URL. [workers.dev](https://developers.cloudflare.com/workers/configuration/routing/workers-dev/). |
| Workers Static Assets | Asset requests are free and unlimited; matching files can be served before Worker code. Invoke Rust only for `/ws`, with no SSR or dynamic host-list API. [Billing](https://developers.cloudflare.com/workers/static-assets/billing-and-limitations/), [routing behavior](https://developers.cloudflare.com/workers/static-assets/). |
| Custom Worker execution | Workers Free allows 100,000 requests/day and 10 ms CPU per invocation. Network waiting is not CPU time. Measure handshake/JWT CPU, message handling, and sustained sessions; do not assume passthrough avoids all resource limits. [Workers limits](https://developers.cloudflare.com/workers/platform/limits/). |
| WebSockets | Available on all Cloudflare plans, but this alone does not prove VPC fetch supports upgrades. [WebSockets](https://developers.cloudflare.com/network/websockets/). |
| Access | Cloudflare advertises a free tier for up to 50 users; the MVP needs one. Configure only the owner's identity. [Cloudflare plans](https://www.cloudflare.com/plans/). |
| Workers VPC | Documents private HTTP fetch and raw TCP via a Tunnel-bound VPC Network; free during beta on all Workers plans. No public Tunnel hostname is used in the proposed route. Beta changes, account availability, local target routing, and workers-rs access need validation. [Binding API](https://developers.cloudflare.com/workers-vpc/api/), [VPC Networks](https://developers.cloudflare.com/workers-vpc/configuration/vpc-networks/), [pricing](https://developers.cloudflare.com/workers-vpc/platform/pricing/). |
| Public Tunnel hostname | A normal public Tunnel application requires an owned domain. This route is excluded from the current no-domain design, rather than treated as a free fallback. [Public Tunnel prerequisites](https://developers.cloudflare.com/tunnel/get-started/). |
| Temporary testing address | Quick Tunnels provide a changing trycloudflare.com URL without an account or domain, but are for development/testing and have no uptime guarantee. They are not the stable deployment plan. [Quick Tunnels](https://developers.cloudflare.com/tunnel/get-started/quick-tunnels/). |

**Alternatives if the preferred probe fails.** The original Option A is a
Rust Worker that relays WebSocket messages directly to VPC TCP `connect()`.
The TCP API is documented for Tunnel bindings; long-lived per-message relay
CPU and memory on Workers Free are not proven. It needs its own measurements
under the same free-only gates. Option B through a public Tunnel hostname
would require a domain and is outside the present constraints. Quick Tunnels
may help a temporary, authenticated test but do not turn into a stable service
by placing a Worker in front. No alternative is an automatic paid fallback.

## Technical Context

**Language/Version**: Stable Rust (edition 2024) for shared protocol code, the
WASM client, and the native WebSocket relay. Browser bootstrapping still uses
the generated JS loader and a small HTML/CSS shell. No TypeScript relay.
The proposed edge upgrade forwarder uses workers-rs; verify its VPC bindings
and upgraded-response handling first. A small wasm-bindgen binding to the
runtime may be needed if the SDK lacks a typed VPC API; do not silently switch
the relay to TypeScript. The preferred route still needs a successful probe.

**Primary Dependencies**: `wasm-bindgen`/`web-sys`, the existing `flate2`,
`des`, and `zune-jpeg` (WASM compatibility to verify), plus a maintained Rust
HTTP/WebSocket library and JWT verifier selected during implementation.
The E2E channel uses `snow` 0.10 (pure-Rust X25519, ChaCha20-Poly1305,
BLAKE2s) on both sides; browser randomness comes from `crypto.getRandomValues`
through `getrandom`.
Deployment uses workers-rs, Wrangler, cloudflared, and Cloudflare Access. Keep
native networking and authentication dependencies out of the shared protocol crate.

**Storage**: Static non-secret connection configuration for one host. No
Cloudflare database. The browser never persists the VNC password or embeds
Access service credentials in assets; non-secret display preferences may persist.
The browser persists only its E2E pairing (pinned host key and its own private
key) in origin storage. The host keeps its private key and the enrolled browser
public keys in ignored local files.

**Testing**: Native Rust unit/integration tests, WASM browser tests, and real
iPhone Safari checks. Preserve the existing malformed-input and bounds tests.
The isolated transport harness passes local tests and a deployed Edge echo
smoke test (before E2E). Its Noise E2E channel passes local Rust and WASM
binding tests. The production TopVNC browser client is not implemented; Safari,
long-session resource behavior, and live E2E remain unverified.

**Target Platform**: iPhone Safari first, with desktop-browser development
checks. Start host validation on Windows. Record exact browser/OS/device
versions before claiming support; iPad, other browsers, and other host OSes
need their own validation.

**Project Type**: Shared Rust library, WASM frontend, native transport adapter,
and free-tier Cloudflare deployment configuration.

**Performance Goals**: Responsive desktop interaction first. Record input-to-
display p50/p95, frame pacing during scrolling/window movement, bandwidth,
host CPU, and memory on Wi-Fi and a mobile link. Compare with a local baseline
using the same source size and quality. Gaming FPS/latency targets do not gate
the personal desktop-control MVP; no performance improvement is claimed yet.

**Constraints**: Free plans and a free workers.dev address only. Bound parser,
framebuffer, WebSocket, and presentation queues, including any edge queues.
Use lower served size/quality on slow links. No public raw-RFB port, required
phone-side VPN, domain purchase, paid upgrade, or paid relay fallback.

**Scale/Scope**: One owner, one configured host, one active web session. No
user administration, sharing, per-user host directory, or multi-tenant service.

## Why not simpler routes

| Route | Verdict |
| --- | --- |
| Worker `connect()` to the host's public IP:5900 | Rejected. It exposes unencrypted RFB to the internet, protected only by VNC's 8-byte password. |
| Worker through private Workers VPC | The no-domain candidate; test HTTP/WebSocket passthrough first, then consider a measured raw-TCP relay. Documentation of HTTP/TCP is not proof of WebSocket passthrough. |
| Host listener through a public Tunnel hostname | Acceptable transport design, but requires an owned domain; excluded from the current deployment constraints. |
| Quick Tunnel as the permanent origin | Excluded from stable deployment. Its temporary address and development-only service do not meet that goal. |
| Cloudflare's browser-rendered VNC | Possible separate experiment, but compatibility with TopVNC and iPhone has not been tested. It would not establish support for the TopVNC web client. |
| Native iOS app | Out of scope. The request is browser-only. |

## AGENTS.md check

| Rule | How this plan complies |
| --- | --- |
| Protocol, framebuffer, input types in reusable crates without UI deps | Phase 1 extracts a sans-IO `topvnc-rfb` crate that native and web both use. |
| Platform transports separate; browser via WebSocket gateway | Native TCP and browser WebSocket adapters wrap the same protocol state machine. The Rust host gateway is transport-only. |
| Presentation separate from decoding | Decode in a browser Web Worker; render through a bounded dirty-region mailbox inspired by `src/presentation.rs`. Apply every update to the authoritative framebuffer before skipping intermediate presentations. |
| Bounded allocations and explicit backpressure | Define byte/message/queue limits. Await native socket writes, throttle RFB update requests, and test negotiated ContinuousUpdates/Fence behavior. Do not assume those extensions alone bound every queue. Close cleanly when limits are exceeded. |
| Untrusted server input | Same validation as native, because it is the same crate. Phase 5 adds fuzzing of the byte-stream parser. |
| Never silently downgrade authentication | The routed browser client requires VNC password auth inside authenticated E2E, plus owner Access and an enrolled browser key. No None or plaintext fallback. The current diagnostic accepts no VNC credentials. |
| Don't claim support before testing | The README and docs stay unchanged until each browser/device combination passes the acceptance criteria. |

## Security model

- **Owner only**: a hostname-based Access application protects the workers.dev
  URL with an exact owner-identity allow rule and default denial. Use OTP or
  the owner's IdP. Disable unused preview/version entry points, or protect
  them equivalently; test that assets and `/ws` cannot bypass Access.
- **Origin authentication**: the Rust Worker validates
  `Cf-Access-Jwt-Assertion` before any VPC request and forwards it to the native
  gateway, which verifies it again before opening RFB TCP. Verify signature,
  team issuer, application audience, expiry, and owner identity. Cache JWKS
  with bounded refresh and handle key rotation; fail closed. Measure edge JWT
  verification within the Free CPU budget. A decoded JWT or email header is
  not proof. [JWT validation](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/http-apps/authorization-cookie/validating-json/).
- **Fixed destination**: the Worker forwards only to one configured private
  HTTP/WebSocket service and the native listener only to its loopback RFB
  target. Do not forward arbitrary paths, follow redirects to other targets,
  or accept an address, port, or URL from browser queries/messages. Prefer a
  single-service VPC binding where upgrade support permits; any broader
  Network binding remains constrained by the Worker allowlist.
- **Upgrade checks**: require the exact web app `Origin`, `/ws` path, and
  `binary` subprotocol. Reject text data messages and excessive message sizes,
  including fragmented messages. Rate-limit failed upgrades locally and
  reject a second live web session instead of silently replacing the first.
- **Session lifecycle**: bound session lifetime by the Access token's expiry;
  reauthenticate through normal navigation when necessary. A dropped socket
  releases held host inputs and does not replay stale input on reconnection.
- **On the wire**: WSS protects browser-to-edge traffic and the Tunnel protects
  edge-to-host traffic. Run cloudflared and the Rust listener on the same
  host. Prove local-only routing for the HTTP/WebSocket hop in Phase 0;
  keep the RFB TCP hop on loopback. Those layers terminate at Cloudflare and
  are insufficient for feature 014's confidentiality requirement. The Noise
  browser-to-host channel above provides it, subject to its stated code
  delivery trust limit and live validation. Retain VNC password authentication
  inside it.
- **Host exposure**: prefer a loopback HTTP/WebSocket target, subject to VPC
  routing tests. If that is unavailable, explicitly validate a same-host
  address and firewall restrictions before choosing it; do not silently add
  an unencrypted LAN hop. No public Tunnel hostname or inbound Internet port
  is used. The new listener is disabled unless the owner opts in.
- **Logging**: bounded connection metadata only (duration, bytes, close reason).
  No passwords, JWTs, clipboard data, framebuffer contents, or input payloads.

## Phases

Each phase ends with a short validation note in this folder, matching the
existing specs' *Validation* sections.

### Phase 0: Free-tier feasibility spike (implementation started 2026-10-30)

1. Confirm a workers.dev address, hostname-based Access protection, and Free
   access to Workers VPC/private Tunnel in the owner's account without adding
   a custom domain or paid plan. This is not a public Tunnel hostname setup.
2. Use isolated throwaway Rust code: a host WebSocket-to-loopback-TCP listener,
   static test page, and workers-rs upgrade forwarder. Verify Rust can access
   the binding and preserve a WebSocket upgrade response. Probe VPC HTTP fetch
   to the service, then WebSocket upgrade/passthrough; record failures separately.
3. Prove local-only target routing from the same-host cloudflared connector,
   including whether loopback works. If a local interface address is needed,
   document the firewall and confirm there is no plaintext cross-host hop.
4. First test only generated echo bytes using `--synthetic-echo`; the binary
   cannot select a desktop target. On an actual iPhone, verify same-origin Access
   login, upgrade, echo, and reauthentication. Wrong-owner/unauthenticated
   requests must fail before private access. RFB/VNC/display/input validation
   is a separate gate after T023's E2E design and implementation pass.
5. Measure cold/warm handshake and JWT CPU against Workers Free limits, whether
   message forwarding invokes Worker code, and two hours of synthetic traffic.
   Also test slow links, memory/queue caps, suspension, and reconnect. Repeat
   relevant resource checks with encrypted desktop traffic only after T023.
   Record latency, CPU, bandwidth, and failure causes. No assumption that
   upgrade passthrough is resource-free substitutes for these measurements.
6. **Gate**: adopt this path only if no-domain deployment, Rust bindings,
   upgrades, authentication, local routing, and Free-plan resource limits all
   pass. Record results in `research.md`. If VPC upgrade passthrough fails,
   a separate raw-TCP Option A probe may be evaluated under the same gates.
   If neither passes, mark the stable free/no-domain design unresolved; do
   not buy a domain, upgrade, or promote Quick Tunnels to a permanent solution.

### Phase 1: Shared sans-IO protocol crate

The client in `src/lib.rs` reads blocking `impl Read` over `TcpStream`, with
threads. Browsers have neither, so:

1. Convert the repo into a Cargo workspace. The root `topvnc` package keeps the
   native app and server; add `crates/topvnc-rfb`.
2. Move the client handshake, security negotiation, VNC auth, message parsing,
   `UpdateDecoder`, Tight/Zlib/Raw decoding, `Framebuffer`, and input encoding
   into `topvnc-rfb` as an incremental push parser, with bounded input, event,
   and output queues. Socket message boundaries must not affect parsing.
3. Gate server-only code (`getrandom`, capture hand-off) out of the crate.
4. The native `Session` becomes a thin TCP adapter over the crate. Preserve
   existing behavior and regression coverage. Measure native latency/frame
   pacing, CPU, and memory before and after extraction under the same workload.
   Investigate regressions before proceeding.
5. CI adds `cargo check -p topvnc-rfb --target wasm32-unknown-unknown`.

This is the largest refactor in the plan. Keep it separately reviewable from
the new gateway and UI, preferably as its own PR when implementation is authorized.

### Phase 2: Rust host gateway and Worker (only for a passing Phase 0 design)

- Add a native-only `crates/topvnc-web-gateway` transport crate, started by an
  opt-in setting in `topvnc --serve`. Keep WebSocket/HTTP concerns outside the
  protocol crate. It relays to one fixed local TCP target.
- Implement the security model above, bounded streaming in both directions,
  heartbeat/idle handling, token-expiry closure, and errors for host offline,
  authentication failure, busy session, and resource limits. Avoid an idle
  policy based only on keyboard input; viewing a desktop is legitimate activity.
- Test missing/forged/expired/wrong-audience/wrong-owner JWTs; bad Origin,
  path, and subprotocol; destination injection attempts; oversized/fragmented
  messages; byte order; slow consumers; second-session rejection; and closure
  propagation in both directions.
- `gateway/cloudflare/` holds the workers-rs upgrade forwarder, Static Assets
  deployment configuration, fixed private service binding, and owner-only
  hostname-based Access setup for workers.dev. No Durable Object or paid
  resource is assumed. Keep matching assets out of custom Worker execution.
- Test edge path/destination restrictions, JWT rejection, preservation of the
  binary subprotocol and Origin, upstream refusal/close, Access expiry, and
  cold/warm Free-plan CPU. Use bounded queues wherever code handles messages.
- Keep protocol and gateway behavior in Rust. If Phase 0 selects raw TCP
  instead of a host WebSocket listener, revise this phase around its measured
  limits and security design before implementation; do not build both paths.

### Phase 3: Web client MVP (`crates/topvnc-web` + `web/`)

- A Rust/WASM crate wraps `topvnc-rfb` in a dedicated browser Web Worker.
  Maintain the authoritative framebuffer there and send bounded dirty-region
  transfers to presentation. Coalesce pending damage rather than dropping
  rectangle updates; a skipped presentation must not leave stale pixels.
- Main thread: a single-host connect screen, VNC password prompt, explicit
  refusal of a server offering only `None` authentication, and
  a canvas viewer with WebGL2 dirty-region uploads and a 2D fallback.
- Schedule rendering with `requestAnimationFrame`. Bound transferable buffers
  and acknowledge consumed presentation work so decoding cannot outpace the UI
  without limit. Do not copy the whole frame for each rectangle.
- Begin with the shared Rust JPEG decoder. Compare alternative browser decoding
  only if profiling identifies a bottleneck and preserves update ordering.
- Advertise only implemented and tested encodings/extensions. Intended coverage:
  Tight, Zlib, Raw, DesktopSize, served-size requests, and negotiated
  ContinuousUpdates/Fence with tested flow control. Pointer modes are enabled
  only where the browser and host behavior are validated.
- Desktop-browser input: mouse, wheel, keyboard by `KeyboardEvent.code`
  mapped to the same US-position keysyms as native, and Pointer Lock where
  the browser allows it.
- In-session panel: disconnect, quality, served size, and a stats overlay
  (fps, bandwidth, RTT).

### Phase 4: Mobile Safari experience

- **Touch modes**:
  - *Trackpad* (default): relative cursor, tap to click, two-finger tap for
    right click, two-finger drag to scroll.
  - *Direct*: absolute tap where you touch.
  - Pinch to zoom and pan the local view.
- **Keyboard**: a hidden input that raises the iOS keyboard, read through
  `beforeinput` and `input` events (the soft keyboard does not give reliable
  `keydown`). Hardware keyboards on iPad use `code`.
- **Toolbar**: Esc, Tab, sticky Ctrl/Alt/Shift/Win-Cmd, arrows, and F-keys.
  Release all held inputs on blur, disconnect, or gesture cancellation.
- **Page lifecycle**: safe-area insets, orientation changes, and optional
  served-size adjustment. Feature-detect fullscreen and wake lock and keep
  ordinary Safari operation usable without either. PWA installation is optional.
- **Backgrounding**: assume tabs can be suspended and sockets can be lost.
  On return, show connection state, reauthenticate with Access if needed, and
  offer reconnect with the VNC password prompt. Never replay buffered input.
- Verify touch gestures, zoom/scroll conflicts, soft-keyboard composition,
  keyboard opening/resizing, and reconnection on the owner's actual iPhone.
  Controller profiles and gaming-specific relative-pointer work are deferred.

Phase 4 is required for the personal MVP; a desktop-only Phase 3 build does not
complete the original mobile-browser goal.

### Phase 5: Performance, hardening, and documentation

- Adapt `examples/latency_bench.rs` or add a browser timing page to measure
  input-to-display latency through the gateway. Report p50/p95 and frame
  pacing for iPhone on Wi-Fi and 5G.
- Fuzz the `topvnc-rfb` push parser and test fixture splits at every offset.
  Include malformed lengths, rectangles, dimensions, and compressed streams.
- Validate the previously defined gateway/client memory caps under slow and
  malicious peers. Verify accumulated dirty regions render correctly after
  deliberately delaying presentation.
- Docs: `docs/cloudflare-gateway.md` with the free-plan deployment runbook
  (workers.dev, Static Assets, hostname-based Access, VPC/private Tunnel,
  Rust gateway), security and reconnect behavior, account prerequisites, and
  a validated-platform table.
  Update the README only for combinations that passed.

### Later phases (outside the personal desktop-control MVP)

- Other users: invitations, per-user host permissions, concurrent sessions,
  audit requirements, and a fresh review of free-tier capacity.
- Gaming: gamepad profiles, pointer lock where available, and measured latency
  targets. Gaming is an enhancement, not an initial acceptance gate.
- Multiple hosts/monitors, clipboard, additional validated platforms, and
  alternative codecs/transports after separate feasibility work.
- A public Tunnel hostname if the owner later already has a domain and changes
  that constraint; paid plans still require a separate scope decision.

## Acceptance criteria (for the finished feature)

- Deployment uses a free workers.dev URL, Free services, and no domain purchase
  or paid subscription. The existing host/Internet connection remains required.
  Phase 0 proves edge CPU/memory/session behavior fits the Free plan; no paid
  service or temporary public URL is used as an automatic fallback.
- The owner's identity can access the app and `/ws`. An unauthenticated or
  different user cannot obtain protected assets or open the RFB TCP connection.
  Missing, forged, expired, and wrong-audience JWTs are rejected (tested).
- The browser cannot choose a network destination. Edge routing reaches only
  the configured private service, and the native relay only its loopback RFB
  target. The host WebSocket listener is opt-in (tested).
- On an iPhone in Safari, a user can sign in, enter the VNC password, see the
  host desktop, move the pointer, click, scroll, type with the on-screen
  keyboard, and disconnect. This is validated on a real device and recorded
  with the iOS version.
- The web client refuses `None`, retains VNC password authentication inside
  verified browser-to-host E2E, and shows where outer TLS terminates. Missing or
  changed host identity fails closed; Access is not payload encryption.
- A host resize (DesktopSize) and a server-pushed update stream work through
  the gateway.
- Gateway and client queues stay under documented byte/message limits with a
  slow phone or paused renderer. Exceeding a limit closes cleanly. Coalescing
  presentation does not discard framebuffer changes (tested).
- Record a two-hour desktop-control soak. Test Access expiry, host offline,
  network loss, and Safari suspension separately; reconnect must not leave
  stuck keys/buttons or replay old input. Cloudflare maintenance disconnects
  are reported, not hidden as application stability.
- Latency, fps, and bandwidth figures are published only from measured runs.
- Native `cargo fmt --all`, `cargo test --workspace`, and
  `cargo clippy --workspace --all-targets` pass after code changes. Record
  comparable native benchmarks before/after the Phase 1 extraction and resolve
  regressions. Check the shared crate for `wasm32-unknown-unknown`.
- The personal MVP does not require multi-user management, gamepad mapping,
  or a gaming-performance claim.

## Proposed structure

```text
specs/015-cloudflare-web-client/
├── plan.md            # this file
├── research.md        # future Phase 0 free-tier and device measurements
└── validation.md      # per-phase validation notes

Cargo.toml             # becomes a workspace; root package stays `topvnc`
crates/
├── topvnc-rfb/        # sans-IO client protocol, decoding, framebuffer, input encoding
├── topvnc-web/        # Rust/WASM client and browser Web Worker entry
└── topvnc-web-gateway/ # native Rust HTTP/WebSocket-to-loopback-TCP transport
web/                   # HTML/CSS, JS loader, optional manifest -> web/dist
gateway/cloudflare/    # workers-rs forwarder, Static Assets and VPC/Access config
docs/cloudflare-gateway.md
```

## Risks

| Risk | Impact | Mitigation |
| --- | --- | --- |
| VPC HTTP fetch cannot preserve WebSocket upgrades, or Rust SDK lacks bindings | Proposed no-domain path fails | Prove both in Phase 0 before extracting the shared crate; evaluate raw-TCP Option A separately if useful. |
| Free Worker handshake/relay exceeds CPU or memory limits | Sessions fail despite a free public URL | Measure cold/warm JWT checks and sustained traffic; fail the gate rather than upgrade. |
| Free-tier terms or eligibility change | Deployment no longer fits the budget | Recheck before deployment and fail the feasibility gate rather than upgrading. |
| Wrong Access application type | Worker-level protection rejects WebSockets | Use hostname-based Access for the exact workers.dev host and test upgrades/expiry in Safari. |
| VPC cannot reach a local-only host listener | Security/routing assumption fails | Test loopback or an explicitly firewalled same-host address; do not open raw RFB publicly. |
| Cloudflare restarts or Safari background suspension | Session drops | Heartbeat, visible connection state, safe input release, and reconnect. Cloudflare documents that network updates can terminate WebSockets. [WebSocket behavior](https://developers.cloudflare.com/network/websockets/). |
| TCP-over-WebSocket on lossy mobile links | Latency spikes and stalls | Lower quality and served size presets for mobile. Consider WebTransport later. |
| Mobile keyboard/gesture differences | Desktop input incomplete | Real-device tests, trackpad/direct modes, modifier toolbar; defer gaming requirements. |
| Phase 1 refactor regresses the native viewer | Native latency worse | A separate PR gated on the existing tests and benches. |
| Transport alone would expose RFB plaintext to Cloudflare | Blocks desktop deployment | Noise XK browser-to-host channel with QR-provisioned host trust is implemented and locally tested; CLI stays synthetic-only until the live E2E and device gates pass. |
| Cloudflare-served client code is substituted | Session and stored browser key readable | Owner-accepted, disclosed limit. Keep account 2FA, sign Wrangler out when idle, and make no confidentiality claim beyond passive relay exposure. |

## Next action

The owner's five design choices and lack of a domain are recorded. A free
workers.dev URL answers the hosting/address question, but it does not prove
the full remote-control path. Phase 0 must establish that path within the
free-only and Rust constraints before the larger refactor starts.

Implementation is authorized. Build and validate Phase 0 in the existing
worktree on the branch created from `main`. The production refactor remains
gated on live Cloudflare and device results; record missing external
prerequisites rather than silently skipping those gates.

As of 2026-10-30, the E2E channel (T023) is implemented locally. Next comes
T026: redeploy the encrypted synthetic probe and run the gateway with the
connector. Then pair the owner's iPhone by QR and verify encrypted echo, which
also covers T011's Safari checks. The resource and two-hour gates follow.
