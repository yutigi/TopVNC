# Validation: Cloudflare browser client

Dates: 2026-10-30 to 2026-10-31. Branch `feat/cloudflare-web-client`, created
from `main` at `03d1881`. **The production client, gateway, and Worker pass
their local tests and a local browser run. The live encrypted route is partly
validated (T026 open). Live desktop control, device, resource, and soak gates
remain open (T011, T017, T020).**

## Desktop-style page and session settings (2026-10-31)

The home page and the viewer's settings panel now follow the desktop app
(`src/ui.rs`): its palette, uppercase labels, square frames, and choices drawn
as rows of buttons. The panel matches the desktop's F8 settings: display row,
served-size slider, quality row, and statistics toggle. A host that does not
offer display selection or served size says so instead of hiding the control.

Local evidence only; nothing was deployed. `npm test` 9 pass and `npm run
build` passes. In the `local_web` harness (Edge on Windows 11, loopback), at
the full window width and at 390 CSS px (CSS zoom, because the maximized window
could not be resized, so media queries still saw the desktop width):

- the quality choice is remembered across a reload and applies in a session;
- the served-size slider at 50% reached the host (harness log), and the panel
  showed "50% shared" after reconnecting;
- the statistics toggle works; the Fn row opens, and the panel moves above it;
- at 390 px: no horizontal page scroll, the quality row wraps 3 + 2, the panel
  scrolls within itself, and the keys scroll with the keyboard and menu
  buttons pinned;
- disconnect closes the panel and resets display and served size to
  unavailable.

Not validated: choosing a display (the harness serves one; only the row's
styling was checked, with injected markup), iPhone Safari and its safe areas,
and the live deployment. Enter in the password field was not checked because
the extension's synthetic Enter produces no `keypress`; sign-in used the
button.

## Production client, gateway, and Worker (T014-T016, T018, T019; 2026-10-31)

All of this is local evidence. Nothing new was deployed, no desktop was
connected through Cloudflare, and no real device was used.

**Rust** (`cargo test --workspace`, same toolchain as below):

- `topvnc-e2e` 10, `topvnc-access` 6: the Phase 0 suites, unchanged.
- `topvnc-web-gateway` 20 and the tunnel launcher 3. These are the Phase 0
  gateway tests plus:
  - the shared RFB handshake, with VNC password authentication, carried
    inside the channel to a password-protected test server, with right and
    wrong passwords;
  - a same-browser reconnect replacing its own stale session;
  - another browser refused with a readable reason, and no TCP connection
    opened, while a session is active;
  - a browser that stops reading: closed after the write deadline, with less
    than 64 MiB accepted from the RFB side.
- `topvnc-web` 11:
  - pairing, the E2E handshake, the RFB handshake, and updates with the
    server's bytes cut into 1-4096-byte pieces;
  - None security refused and tampered messages rejected;
  - continuous updates, fence replies after the preceding update, and resize
    requests;
  - fence round-trip measurement;
  - input clamped to the framebuffer, with release of everything held;
  - bounded message sealing;
  - physical-key keysyms matching the native table, and Shift for text;
  - damage that stays bounded while presentation is skipped and still covers
    every changed pixel.
- Clippy clean for the workspace (`--all-targets --all-features`), for
  `topvnc-rfb`, `topvnc-e2e`, and `topvnc-web` on `wasm32-unknown-unknown`,
  and for the Worker.

**Web** (Node 22.18.0): `npm test` 9 pass (gestures, view transform, pairing
storage); `npm run build` (WASM 409,317 bytes, page bundle 32 KB, Worker
bundle 12 KB); `npm run test:wasm` 2 pass against the built bindings.

**Worker**: `worker-build` 0.8.7 packaging passes. `wrangler deploy --dry-run`
passes with the example config, and with the owner's ignored local config,
which binds the existing VPC service: 6 assets, 609.03 KiB (230.32 KiB gzip).

**Browser, local harness** (`local_web` example; Edge 156 on Windows 11,
driven through the Claude in Chrome extension; loopback only):

- pairing from a `#pair=` link pasted into an open tab (`hashchange`), with
  the fragment removed from the address bar;
- enrollment saved after the first decrypted host message;
- password prompt and sign-in;
- the animated scene drawn by the Worker through OffscreenCanvas;
- the server received the mouse click, the keys, and the wheel notches;
- statistics showed 30 updates and 30 draws per second, about 0.4 Mbit/s,
  continuous updates, and a fence round trip;
- the 50% served size applied through DesktopSize;
- disconnect, then reconnect with the saved pairing;
- a wrong password rejected with its reason;
- a second tab replacing the first, which showed why;
- the page-thread fallback (OffscreenCanvas removed) connected and drew;
- no console errors.

Bugs found and fixed during this run:

- `display` rules overrode the `hidden` attribute, so the viewer covered the
  page;
- the harness's test signing keys went stale after five minutes;
- the messages for a viewer-initiated close and a refused password were
  misleading.

Not validated: the live Cloudflare route with the production client, touch
and the on-screen keyboard on a device, iOS lifecycle, and mobile networks.

## Shared protocol crate extraction (T012, T013; 2026-10-31)

Started after the live encrypted route had passed in iPhone Safari and Edge
(T026, below), while T026's unenrolled-browser check and T011's resource and
two-hour gates were still open. Phase 0 had already selected the route, so
the gates no longer decide whether to extract; they still gate any desktop
deployment. The extraction is local, and the native viewer keeps working
throughout.

**What moved.** `crates/topvnc-rfb` holds the client protocol without
sockets, threads, or UI: a push state machine for the handshake
(`ClientHandshake`, with the blocking `negotiate_with_encoding` rebuilt on
it), incremental server-message framing (`MessageReader`), Raw/Zlib/Tight
rectangle decoding (`RectDecoder`, Tight decoder, JPEG), `Framebuffer`,
input state and encoding (`InputState`, message builders), and the protocol
numbers the server shares. The root `topvnc` crate re-exports the same public
names. It keeps the server, the Tight encoder, the TCP session, and the
JPEG decoder threads, which now receive complete rectangles from the reader.
`MessageReader` validates every header, length, encoding, and rectangle
against the current framebuffer before it waits for the body. It can buffer
at most one maximal rectangle plus a 256 KiB read-ahead, or less under a
viewer pixel limit.

**Tests** (Rust 1.95.0, Windows 11, Ryzen 9 9950X3D, 32 logical CPUs):

- `cargo test -p topvnc-rfb`: 22 pass. The tests cover:
  - a stream of every message kind, cut at every chunk size from 1 to 63 and
    split at every byte offset, with identical messages and pixels;
  - malformed headers rejected before their bodies arrive;
  - DesktopSize bounds, a viewer pixel limit, and the buffer limit;
  - exact reads that never consume past a message;
  - handshake chunking, leftover bytes, None refusal, rejection reasons,
    and oversize limits;
  - Tight body lengths for every kind at every prefix;
  - held-key release and its bound.
- `cargo test --workspace`: the library has 82 pass, 1 fail, and 1 ignored;
  the binary has 93 pass and 6 ignored. The one failure is the pre-existing
  `clients_without_desktop_size_are_disconnected_on_resize` (`Timeout`).
  Unchanged `HEAD` (`8321675`, exported to a scratch directory) gives the
  same failure, with 85 library tests passing. The difference is the three
  decoder-only Tight tests that moved into `topvnc-rfb`.
- `cargo clippy --workspace --all-targets`: no warnings. `cargo check` and
  clippy for `topvnc-rfb` on `wasm32-unknown-unknown`: pass.

**Native benchmarks** (release, below-normal priority; the owner's
`topvnc` session was running). The "before" binaries were built from
`8321675` and kept frozen, then rerun after each "after" run to expose
drift. The latency rows are `latency_bench` publish-to-decode: mean / p95
latency and delivered fps, two runs each. They exclude capture, GPU, and
display.

| Case | Before (3 sessions) | After, first version | After, bounded read-ahead |
| --- | --- | --- | --- |
| 1080p framebuffer apply, mean | 0.382–0.397 ms | 0.387 ms | 0.383–0.384 ms |
| Tight q6 push, direct, 120 fps | 4.27–4.33 / 5.24–5.33 ms | 4.42, 4.39 / 5.46, 5.39 ms | 4.24, 4.26 / 5.30, 5.31 ms |
| Raw push, direct, 60 fps | 4.59–4.74 / 5.42–5.70 ms | 5.52, 5.90 / 15.5, 19.5 ms; 59.4 fps once | 4.61, 4.56 / 5.42, 5.49 ms |
| Tight q6 adaptive, 100 Mbit/s | 34.6–35.0 / 42.1–42.9 ms, 35 fps | 34.5, 34.8 / 42.0, 42.5 ms | 34.6, 34.6 / 42.3, 42.3 ms |

The first version regressed Raw: greedy reads could run far ahead of the
current message, so large rectangles were moved and the buffer doubled.
Reads now go at most 256 KiB past the next message, the old `BufReader`
size. Room for a whole rectangle is reserved once, when it starts. The final
version is within the before-run spread on every row. Process CPU seconds
and peak working set were also within that spread, for example Tight 25.3–25.5
CPU seconds after against 24.0–27.2 before. These are short loopback
runs; no performance improvement is claimed. Raw logs are in ignored
`target/bench-015-*.txt`.

## Live encrypted probe checks (T026, 2026-10-30; in progress)

Details and close reasons are in research.md.

- Local checks before each deploy: `npm test` 8/8, `npm run build`,
  `npm run test:wasm` 3/3, and `deploy:check` (5 assets, 609.22 KiB). The
  second run followed the `probe.mjs` fix.
- That fix makes the first sample wait one interval after the handshake. It has
  no automated test: the page script has no DOM harness, and the Node tests
  only cover its helper modules. The live route verified it.
- Passed live on the deployed route:
  - encrypted echo: iPhone Safari (iOS 27.0.1, 185 samples) and Edge 156
    (46 samples);
  - plaintext and forged-handshake clients rejected without data;
  - wrong pinned host key rejected without data;
  - consumed invite rejected.
- Pending: unenrolled-browser rejection on the live route. Expired-invite
  rejection is covered by local tests only.

## Browser-to-host E2E checks (T023, 2026-10-30)

Noise XK/XKpsk3 channel with QR pairing, in the isolated spike workspace only.
The owner accepted the disclosed code-delivery trust limit (plan.md). Rust
1.95.0, Node 22.18.0, `snow` 0.10.0, `wasm-bindgen` 0.2.129.

- `cargo fmt --all -- --check`: pass.
- `cargo test --workspace --locked`: pass, 34 tests: 10 E2E, 2 WASM-binding
  (native), 16 gateway, 6 policy. Tests cover:
  - session and pairing handshakes;
  - wrong pinned host key, substituted or altered host reply, and an
    unenrolled browser;
  - wrong, expired, and reused invites, where a failed guess does not consume
    the invite;
  - header and mode tampering and fixed handshake sizes;
  - transport replay, reordering, loss, tampering, and truncation, after which
    the channel stays closed;
  - plaintext and size bounds, and ciphertext-only relay output;
  - no TCP connection before an authorized handshake, a ten-second handshake
    timeout, and an offline-target close reason;
  - enrollment persistence and key-file validation.
  The earlier relay regressions were kept and now run inside the channel:
  Access/Origin/query rejection, one active session, text, oversized and
  fragmented messages, idle resume, and session expiry.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: pass.
  `wasm32-unknown-unknown` clippy for the worker, browser, and E2E crates: pass.
  The E2E dependency tree for WASM contains no `ring` or C build.
- `npm test`: eight Node tests pass (adds bounded pairing storage and fragment
  parsing). `node --check` passes for the probe and storage modules.
- `npm run build`: pass. Browser WASM 149,787 bytes, probe bundle 12,088
  bytes. `npm run test:wasm`: three smoke tests of the built bindings pass
  (keys, fingerprints, invite parsing, bound headers, forged-reply failure).
- `wrangler deploy --dry-run --config wrangler.example.toml`: pass, five static
  assets, 609.22 KiB total upload (230.34 KiB gzip). Nothing was deployed.
- Host CLI smoke test with fictional values and a loopback ephemeral port:
  - help exits 0; the legacy config-only invocation exits 2;
  - a missing host key fails with an `--init-host-key` hint;
  - `--init-host-key` creates the key once and refuses to replace it;
  - `--pair` prints the fingerprint, a terminal QR code, and the one-time link.
  The process was then stopped; no listener remained.

Not validated: the bundled page against the deployed route, QR scanning, Safari
storage/WASM/CSP behavior, and Free-plan CPU with E2E traffic (T026, T011).
The root native package is a separate Cargo package and is unchanged, so the
native suite was not rerun.

## Recovery revision checks

On the restored worktree after converting the live probe to synthetic echo:

- Spike `cargo fmt --all -- --check`: pass.
- Spike `cargo test --workspace --locked --offline -j 4`: pass, nine host tests
  and six policy tests (15 total). The added test covers bounded multi-chunk
  echo and a subsequent session.
- Spike `cargo clippy --workspace --all-targets --locked --offline -- -D warnings`:
  pass.
- Browser Node tests: five pass, including split echo validation and rejection
  of altered, stale, excess, empty, and unsolicited data.
- `node --check probe.mjs`: pass. Safari 17-targeted esbuild bundle: pass.
  This is compilation evidence, not Safari/device execution.
- CLI help succeeds; the legacy config-only invocation exits with status 2
  before loading configuration or starting a listener.
- `npm ci --offline`: restored pinned tooling successfully after requesting
  sandbox access to the existing npm cache; no dependency version changed.
- Spec Kit prerequisites resolve `specs/015-cloudflare-web-client` using
  explicit feature context. No extension hooks or feature checklists exist.

The executable requires `--synthetic-echo` and replaces the saved TCP target
with an ephemeral loopback echo listener. The browser requests no password and
imports no noVNC code. Subsequent live synthetic deployment and connector
startup are recorded below; no desktop session was started. The root native
source/Cargo files remain unchanged; old native
results below apply only to `c9c33f5`, not the newer main baseline.

## Original isolated workspace checks (before recovery)

In `gateway/cloudflare/spike/`:

- `cargo fmt --all -- --check`: pass.
- `cargo test --workspace`: pass, eight native gateway integration tests and
  six shared policy tests. No tests ignored.
- `cargo clippy --workspace --all-targets -- -D warnings`: pass.
- Worker `wasm32-unknown-unknown` compilation and worker-build packaging: pass.
- In `worker/`, `npm test`: pass, three tests covering refusal of offered None,
  server-selected None, nested security schemes, and incompatible adapter APIs.
- `wrangler deploy --dry-run --config wrangler.example.toml --outdir
  build/dry-run`: pass. The WASM/JS package and static browser bundle are built;
  the example VPC binding is recognized. This command did not deploy resources.

Gateway tests cover rejected auth/Origin/query injection without opening TCP,
ordered binary forwarding, text/oversized/fragmented message rejection,
single-session enforcement and slot release, TCP/WebSocket closure, session
expiry without peer activity, and resuming traffic after an idle interval.

Policy tests cover signature tampering, wrong owner/audience/issuer, missing or
expired expiry, future not-before, algorithm/critical-header rejection,
oversized tokens/JWKS, and fixed private upstream restrictions.

The original temporary browser harness used pinned noVNC, not the production
Rust/WASM TopVNC client. It was subsequently replaced by synthetic diagnostics.
No browser visual test or real-device test has run yet.

## Historical native application baseline (`c9c33f5`)

The root Rust source, Cargo.toml, and Cargo.lock are unchanged.

- `cargo fmt --all -- --check`: pass.
- `cargo clippy --workspace --all-targets`: pass.
- Unfiltered `cargo test --workspace`: the existing library test
  `clients_without_desktop_size_are_disconnected_on_resize` fails with
  `Timeout` at `src/lib.rs:5035`. The other 82 library tests pass, one is ignored;
  Cargo stops before the remaining binaries. This same failure is documented
  in the pre-existing `specs/012-compact-title/spec.md` validation.
- With that test excluded, `cargo test --workspace -- --skip
  clients_without_desktop_size_are_disconnected_on_resize`: pass, 159 tests
  passed, three intentionally ignored, one filtered out.

No native benchmark or performance improvement is claimed. The production
protocol extraction has not started.

## External prerequisites

Wrangler login succeeded, but its OAuth token receives HTTP 403 for billing
and Access organization inspection. Workers Free and Zero Trust Free are now
confirmed in the dashboard, and owner-only hostname Access is saved. The
synthetic Worker is deployed; no public Tunnel hostname, desktop route, or
paid resource was deployed.

Edge control subsequently worked after approval and foreground activation.
The Workers plans page confirms Free as the current plan. After explicitly
approving Free selection, the owner personally reviewed/completed Zero Trust
checkout; Overview confirms **Plan: Zero Trust Free**. The Access application
form was initially blocked by automatic approval review. The owner completed
the exact-email policy manually and asked to proceed. Verified the single
owner-email Allow rule, saved the hostname application with a six-hour duration,
and verified its saved settings/AUD in the dashboard. Non-secret account values
are in ignored local TOML files. The connector token file has owner/SYSTEM-only
Windows permissions.

Restored the pinned worker-build tool, rebuilt WASM/browser assets, and passed
Wrangler dry-run with the real private VPC binding. Initial live deployment
failed with a future UTC compatibility date; changing it to 2026-10-29 allowed
deployment. Unauthenticated GET `/` and `/ws` both return 302 to the expected
team Access login. Temporary synthetic gateway and QUIC connector processes
are running for the authenticated test; no desktop or service installation.

The authenticated Edge probe passed VPC WebSocket upgrade and loopback routing:
258 generated 1 KiB samples in approximately 4 minutes 22 seconds, with normal
close and matching upstream/downstream counts. The desktop probe is stopped
to free the one session slot for the owner's iPhone test. Listener inspection
confirms only `127.0.0.1:6080`. GraphQL returned one successful sampled
invocation at 9.155 ms CPU (units confirmed by API schema); this single row does
not establish the cold/warm or long-session gate. Two-hour sessions, slow-link
memory, repeated edge CPU, browser-to-host E2E, and iPhone Safari remain unverified.
See research.md for the pending gate matrix.
