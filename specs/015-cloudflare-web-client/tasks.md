# Tasks: Personal browser client on workers.dev

## Setup

- [X] T001 Record accepted scope, implementation authorization, and Phase 0 gating in specs/015-cloudflare-web-client/spec.md and plan.md.
- [X] T002 Create an isolated Rust workspace for the feasibility harness in gateway/cloudflare/spike/Cargo.toml and verify .gitignore covers its artifacts/secrets.

## Foundational: Phase 0 harness (required before production changes)

- [X] T003 Add focused JWT and upgrade-policy tests in gateway/cloudflare/spike/policy/src/lib.rs.
- [X] T004 Implement bounded RS256/JWKS verification and strict upgrade/destination policy in gateway/cloudflare/spike/policy/src/lib.rs.

## US1: Secure connection feasibility (P1)

- [X] T023 [US1] Specify browser-to-actual-host E2E transport, independently provisioned host identity, and the trust limit of Cloudflare-served client code in specs/015-cloudflare-web-client/plan.md; implement and validate it before desktop traffic. (Noise XK/XKpsk3 with QR pairing in gateway/cloudflare/spike/e2e/, browser/, host/, and worker/; owner accepted the trust limit on 2026-10-30; local Rust/WASM validation only, live check is T026.)
- [X] T024 [US1] Reconcile the restored harness with current main and rerun relevant local checks; complete Workers Free / Zero Trust Free verification and owner-only hostname Access setup without exposing a desktop.
- [X] T025 [US1] Replace the live browser password/desktop probe with bounded synthetic echo diagnostics and force a generated loopback target in the host CLI; retain local relay regression tests.
- [X] T005 [US1] Add native WebSocket/TCP integration tests for auth rejection, ordered binary relay, size limits, busy sessions, and close propagation in gateway/cloudflare/spike/host/src/tests.rs.
- [X] T006 [US1] Implement the fixed-loopback Rust gateway, expiry, timeouts, and backpressure in gateway/cloudflare/spike/host/src/lib.rs and main.rs.
- [X] T007 [US1] Implement the workers-rs VPC upgrade forwarder with JWT verification and fixed upstream in gateway/cloudflare/spike/worker/src/lib.rs.
- [X] T008 [US1] Add the browser connection probe, Free-plan Worker configuration, and private-Tunnel setup in gateway/cloudflare/spike/worker/public/, wrangler.example.toml, and gateway/cloudflare/spike/README.md.
- [X] T009 [US1] Run native checks, Worker WASM build/deployment packaging, and browser-probe checks; document results in specs/015-cloudflare-web-client/validation.md.
- [X] T010 [US1] Deploy only the authenticated synthetic Free-plan probe through the owner's account and verify VPC WebSocket upgrades/local routing in specs/015-cloudflare-web-client/research.md. Do not connect a desktop.
- [ ] T026 [US1] Redeploy the encrypted synthetic probe, create the host key, pair the owner's browser by QR, and verify encrypted echo, rejection of an unenrolled browser, and no plaintext fallback over the live route; record in specs/015-cloudflare-web-client/research.md. Do not connect a desktop. (In progress 2026-10-30: deployed; iPhone and Edge encrypted echo, plaintext client, wrong host key, and reused-invite rejection passed live; a probe start bug was fixed and redeployed. Unenrolled-browser check remains.)
- [ ] T011 [US1] Verify real iPhone Safari login/RFB traffic, Free CPU limits, slow consumers, and two-hour sessions; record the architecture gate in specs/015-cloudflare-web-client/research.md.

## US2: Production desktop control (P1; blocked until T010–T011 pass)

- [X] T012 [US2] Capture native performance baselines in specs/015-cloudflare-web-client/validation.md before extracting crates/topvnc-rfb/.
- [X] T013 [US2] Extract the bounded incremental Rust protocol crate and preserve native TCP behavior in Cargo.toml, crates/topvnc-rfb/, and src/lib.rs. (2026-10-31: started after T026's live encrypted echo passed; native tests and benchmarks unchanged, see validation.md.)
- [X] T014 [US2] Integrate only the Phase 0-selected production gateway in crates/topvnc-web-gateway/ and gateway/cloudflare/. (2026-10-31: crates/topvnc-e2e, topvnc-access, topvnc-web-gateway and gateway/cloudflare/worker; relays to `topvnc --serve` on loopback; local tests and Wrangler dry run only, not deployed.)
- [X] T015 [US2] Implement the Rust/WASM decoder, accumulated dirty-region presentation, and one-host connect UI in crates/topvnc-web/ and web/. (2026-10-31: Worker + OffscreenCanvas 2D with page-thread fallback; validated in Edge through the local harness only.)
- [X] T016 [US2] Implement touch modes, keyboard/modifier toolbar, zoom, and orientation handling in crates/topvnc-web/ and web/. (2026-10-31: implemented and unit-tested; device behavior is T017.)
- [ ] T017 [US2] Validate the complete desktop-control scenario on real iPhone Safari in specs/015-cloudflare-web-client/validation.md.

## US3: Interruption recovery and resource control (P2; depends on US2)

- [X] T018 [US3] Implement Access-expiry/reconnect and safe held-input release in crates/topvnc-web/ and the selected gateway. (2026-10-31: same-browser reconnect replaces its stale session; release on blur, hide, cancel, and disconnect; expired sign-in detected; tested locally.)
- [X] T019 [US3] Test malformed/chunked protocol data, paused presentation, slow peers, and lifecycle interruptions in crates/topvnc-rfb/ and crates/topvnc-web/.
- [ ] T020 [US3] Record two-hour soak and Wi-Fi/mobile input latency, frame pacing, CPU, and memory results in specs/015-cloudflare-web-client/validation.md.

## Polish

- [ ] T021 Write the tested Free-plan deployment runbook and supported-device matrix in docs/cloudflare-gateway.md; update README.md only for validated combinations. (2026-10-31: runbook and matrix written; the live deployment steps are not yet tested, so README.md is unchanged.)
- [ ] T022 Run final workspace/WASM checks and compare native benchmarks in specs/015-cloudflare-web-client/validation.md.

## Dependencies and execution strategy

T001–T004 precede US1. T005 precedes T006; T004 precedes T007.
T006–T008 precede T009; T009 precedes T010–T011. T023 is additionally required
before any live task carries RFB credentials or desktop traffic, and T026
validates it on the deployed route before T011's device checks. T025 allows
T010's synthetic routing diagnostics without desktop/E2E traffic. T024
must establish current account/configuration and local evidence first.
Cloudflare login, Free-account
availability, and a real iPhone are external prerequisites for T010–T011.
Do not mark those gates complete based on a local test or compilation.

US2 depends on the passing live gate; US3 depends on US2. Production work
remains listed so the end goal is explicit, rather than treating a probe as
the finished browser client.

Independent-file opportunities after T004: host tests/implementation (T005–T006)
and Worker/probe (T007–T008) can be developed independently. After extraction,
US2 presentation and input can be developed separately against the protocol
contract. Execute sequentially in this session; no agent delegation is needed.
