# Personal TopVNC browser client

## Scope

The owner controls one host from iPhone Safari using a free workers.dev URL.
Use Rust for protocol/client and relay code. Cloudflare services must remain
on Free plans; no custom domain purchase, phone VPN, or public raw-RFB port.
Desktop control is the MVP. Other users and gaming features come later.

Implementation was authorized on 2026-10-30. Start with the isolated Phase 0
spike in plan.md. Do not start the production protocol extraction until the
live free-tier, private-Tunnel, authentication, and Safari gates pass.

The current main branch also requires end-to-end encryption before Cloudflare
carries a desktop session (feature 014). Apply that requirement here: the browser
must authenticate the actual host independently, and RFB credentials, frames,
input, and clipboard must stay encrypted through Cloudflare. The harness now
has a Noise XK browser-to-host channel with QR-provisioned host trust, tested
locally only (plan.md, T023). On 2026-10-30 the owner accepted its disclosed
limit: Cloudflare serves the client code and could substitute it. Keep desktop
deployment blocked until the live encrypted route (T026) and device gates pass.

## User stories

### US1 (P1): Establish a secure free connection path

As the owner, I can sign in at my workers.dev URL and reach only my configured
host through a private Tunnel. First prove the route with a Rust feasibility
harness before changing native TopVNC behavior.

Independent validation: automated authentication and WebSocket/TCP tests,
Worker WASM compilation, then a real Cloudflare/Safari deployment test.

### US2 (P1): Control the desktop from Safari

As the owner, I can see the desktop, move/click/scroll the pointer, type with
the phone keyboard, resize the view, and disconnect using the shared Rust RFB
client. This depends on US1's live feasibility gate.

Independent validation: protocol chunking/bounds tests and the real-device
desktop-control acceptance scenario.

### US3 (P2): Recover safely from interruptions

As the owner, I can see a disconnected or expired-login state and reconnect
without stuck keys, stale input replay, or unbounded memory growth.

Independent validation: slow-peer, expiry, background/foreground, and network
loss tests, followed by a two-hour desktop-control soak.

## Acceptance criteria

- Use only a free workers.dev address and Free services; report beta
  dependencies and remaining unverified limits explicitly.
- Access permits only the owner. Validate JWT signature, issuer, audience,
  expiry, and identity before private/RFB access. Never treat a decoded token
  or unverified header as authentication.
- Neither browser URL parameters nor messages choose the relay destination.
  Validate Origin, path, binary subprotocol, message sizes, and framebuffer data.
- Keep native TCP, WebSocket transports, protocol state, and presentation separate.
- Bound all queues/allocations and handle slow peers with backpressure or a
  clean close; do not silently discard incremental framebuffer state.
- Retain VNC password authentication inside an authenticated browser-to-host
  encrypted channel. Access and Cloudflare's TLS/Tunnel termination do not
  provide this confidentiality. Reject missing/changed host identity without a
  plaintext fallback. Define and test the browser trust/bootstrap model before
  carrying desktop data.
- Complete touch, keyboard, and reconnect checks on a real iPhone before
  describing mobile support as implemented.
- Preserve native behavior and complete the Rust checks in AGENTS.md.

## Validation

The isolated Phase 0 harness, including its E2E channel, passes its local
Rust/Node/WASM-binding tests and Worker packaging dry run. Its encrypted echo
passed live in iPhone Safari and Edge. On 2026-10-31 the production pieces
(shared protocol crate, gateway, Worker, and WASM client) passed their local
tests and a local Edge run through a test harness, with native behavior and
benchmarks unchanged. Actual results and the existing native-suite timeout are
recorded in validation.md. The live desktop-control, Cloudflare resource,
two-hour, and iPhone gates remain pending; local or mocked tests do not prove
those gates.
