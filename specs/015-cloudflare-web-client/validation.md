# Validation: Cloudflare browser feasibility harness

Date: 2026-10-30. Current branch `feat/cloudflare-web-client`, created from
`main` at `03d1881`. Recovered the original uncommitted harness from its stash
without dropping that backup. **Local synthetic harness validated; live
Cloudflare/Safari and browser-to-host E2E gates remain open.**

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
