# Phase 0 feasibility research

Date: 2026-10-30. **Live architecture gate: pending.**

## Implemented local probe

`gateway/cloudflare/spike/` is an independent Cargo workspace. It contains a
Rust JWT/routing policy crate, native WebSocket-to-loopback-TCP relay, and
workers-rs VPC HTTP upgrade forwarder. The browser now verifies generated
1 KiB echo samples with one outstanding sample, chunk-aware validation,
timeouts, and a bounded log. The CLI only accepts `--synthetic-echo` and
substitutes its own ephemeral loopback target, ignoring the saved RFB target.
The original noVNC password-only adapter remains locally tested but is not
imported into the bundle. No production protocol extraction or native session
change has started.

Since that deployment, the probe gained the browser-to-host Noise channel
(T023; design in plan.md). The browser now pairs by QR code and sends only
ciphertext through the Worker, VPC service, and Tunnel; the gateway decrypts
on the host. The deployed Worker still serves the earlier plaintext probe until
it is redeployed for T026.

## Account observations

- Wrangler OAuth sign-in succeeded. The account has a workers.dev subdomain.
- VPC service listing succeeded and returned no services. Tunnel listing
  succeeded and returned no tunnels at the time of inspection.
- Subscription and Access organization API reads returned HTTP 403 under the
  Wrangler token. This is a permissions limitation, not proof of a paid/free
  plan or absence of Zero Trust.
- The Free plans were subsequently confirmed in the dashboard (see below).
  Owner-only hostname-based Access still needs setup. Do not deploy a host route
  based on assumptions.
- The first dashboard automation attempt stopped because the computer-use
  runtime could not confidently identify the browser's current URL. No billing
  or Access settings were changed by that attempt.
- On retry, the account home was readable in Edge, but navigating to Workers
  & Pages failed with `Computer Use app approval timed out`. The Workers plan
  and Access configuration remain unverified; no cloud settings were changed.
- After Edge approval and bringing its window to the foreground, the Workers
  plans screen confirmed **Free / $0 / Current plan**. No Workers upgrade is
  needed or selected.
- Zero Trust already has a team domain, but its settings explicitly say no
  plan has been chosen. Its plan picker offers Zero Trust Free at $0 per
  seat/month with a 50-seat limit. Selecting that plan was blocked by automatic
  approval review because it may activate a subscription without explicit
  approval. The owner subsequently approved selecting Free, then personally
  reviewed and completed checkout. The Overview screen now confirms
  **Plan: Zero Trust Free**. No paid Workers upgrade was selected.

## Worktree recovery and security reconciliation

Another session's authorized cleanup preserved the implementation in a stash
and removed its worktree. Restored the saved files into
`feat/cloudflare-web-client`, created from current `main` at `03d1881`.
The stash backup remains. Renumbered this feature to 015 because 013 now holds
multi-monitor and 014 holds the native E2E-first Cloudflare plan.

Feature 014 requires E2E before Cloudflare carries RFB. The restored transport
probe does not satisfy that requirement. No desktop route or connector was
deployed. Browser-to-actual-host encryption and independent host identity,
including the trust limit of provider-served browser code, are a deployment
gate in addition to Access and Free-tier validation.

Access setup is now saved. The owner manually completed the email policy, then
asked to proceed. Verified its single Include rule is the owner's exact email,
with Allow action and no other policy. Saved the hostname-based application
for the planned workers.dev hostname with a six-hour session duration. The
dashboard confirms successful configuration; reopening it confirms the saved
duration. Retrieved the application AUD tag from Additional settings and placed
it with the issuer/owner/account values in ignored local TOML configs.

Created a dedicated remotely managed private tunnel and a single HTTP VPC
service pointing to connector loopback `127.0.0.1:6080`. No public Tunnel
hostname, private network route, RFB port, or firewall rule was added. The
connector token is in an ignored file whose Windows ACL permits only the owner
and SYSTEM. Cloudflare's Workers VPC pricing page still says free during open
beta; the Workers Free plan remains the selected compute plan.

## Build findings

- workers-rs 0.8.7 has a generic environment binding mechanism, but no typed
  Workers VPC API. The probe uses a small wasm-bindgen binding for the documented
  `fetch()` method, with a fixed private upstream and no message handlers.
- Rust WASM checking succeeded. `strip = true` broke wasm-bindgen packaging
  with `externref table required for catch wrappers`; preserve metadata with
  `strip = "debuginfo"`. [Upstream issue](https://github.com/cloudflare/workers-rs/issues/1014).
- A pending-upgrade timeout is canceled when the upgrade completes, so it does
  not deliberately abort an accepted long-lived session.

## Live gates that remain unproven

The Rust Worker and synthetic static page are now deployed at the planned
workers.dev hostname. Initial deployment failed with API 10021 because
`2026-10-30` was still a future UTC date; corrected the compatibility date to
`2026-10-29` and deployment succeeded. Wrangler reported 4 ms Worker startup
time; that is not measured JWT request CPU or stream performance.

Unauthenticated HTTP requests to `/` and `/ws`, with redirects disabled, both
return 302 to the configured team's Access login. This establishes login
interception for those requests, not rejection of a signed-in wrong owner or a
passing WebSocket upgrade. Previews remain disabled.

Started a temporary loopback-only synthetic gateway and outbound QUIC connector
for the authenticated route test. No Windows service was installed. Their
process lifetime and cleanup must be recorded when testing ends.

The tunnel API reports **healthy**, with four QUIC connections. After the owner
completed Access sign-in, the Edge probe verified more than 250 generated samples
through the deployed route. This proves binary WebSocket upgrade passthrough
over the HTTP VPC service and connector-to-loopback reachability for this exact
deployment. The gateway verifies the real Access JWT before connecting to its
own generated echo target. The displayed echo RTT is a synthetic diagnostic,
not a desktop/input-to-display measurement. iPhone testing and the two-hour
resource/lifecycle gate remain open.

Stopped the Edge session after approximately 4 minutes 22 seconds to free the
single session slot for an iPhone test. The gateway logged normal closure
(1000), with 264,192 bytes in each direction (258 full 1 KiB samples). The
stopped state was visually verified. The synthetic service/connector remained
running for the owner's device test. When the E2E work began later on
2026-10-30, neither a gateway nor a `cloudflared` process was running and
nothing listened on port 6080. No iPhone result was recorded. Restart both for
T026 with the new gateway, which needs a host key and the two key-file settings.

The GraphQL metrics API is readable with the existing Wrangler credential.
Its first returned row records one successful invocation, no errors, and
`cpuTimeP50 = cpuTimeP99 = 9155`. The API's schema descriptions confirm these
fields use microseconds (9.155 ms). This is one sampled invocation, not a
cold/warm or long-session resource gate; leave that gate open. A single process
snapshot after the desktop test showed gateway working set 16,424,960 bytes
and cloudflared 41,824,256 bytes. This is not peak memory or a leak/soak test.
Windows listener inspection confirmed only `127.0.0.1:6080` for the gateway.

| Gate | Result |
| --- | --- |
| Workers Free account | Confirmed current plan in dashboard |
| Zero Trust Free account setup | Confirmed active after owner completed checkout |
| Browser-to-actual-host E2E | Noise XK + QR pairing implemented and locally tested; owner accepted the code-delivery trust limit; live route check pending (T026); desktop still blocked |
| Owner-only hostname-based Access for workers.dev | Saved; exact owner-email rule and six-hour duration verified; unauthenticated `/` and `/ws` redirect to team login; owner session passed, live wrong-owner session untested |
| Private VPC binding + same-host Tunnel connector | Dedicated service running over healthy QUIC tunnel |
| HTTP/WebSocket upgrade passthrough | Passed basic authenticated Edge echo test on deployed Rust Worker/VPC service |
| Loopback target reachability | Passed for `127.0.0.1:6080`; target stayed loopback-only |
| Wrong-owner/unauthenticated request isolation at the edge | Pending |
| Cold/warm Worker CPU within Free limits | One sampled successful invocation: 9.155 ms; repeated cold/warm and long-session validation pending |
| Two-hour session and slow-link memory behavior | Pending |
| Real iPhone Safari auth/display/input/lifecycle | Pending |

The local tests do not establish any of these live results. No performance
or supported-platform claim is made, and the production refactor stays gated.
