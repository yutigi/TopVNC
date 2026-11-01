# Phase 0 feasibility research

Dates: 2026-10-30 to 2026-10-31. **The live route carried desktop control to
an iPhone (2026-10-31). The two-hour and Free-plan CPU gates are still open.**

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
on the host. The encrypted probe was deployed for T026 on 2026-10-30 and
replaced the plaintext probe (below).

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

## Encrypted route check (T026, 2026-10-30; in progress)

Times are +07:00. No desktop was connected; the gateway only reached its own
loopback echo target.

**Setup.**

- Synced the second checkout to `d706f2f` (its index matched `3c7a909`, so
  nothing was lost) and added `host_key_file` and `browser_keys_file` to the
  ignored host config.
- `--init-host-key` created the host key with fingerprint
  **`a1d1-5d59-ad96-ab17`**. The CLI does not restrict the file, so `icacls`
  limited `host.key` to the owner and SYSTEM, like the tunnel token.
- Before deploying: `npm test` 8/8, `npm run build`, `npm run test:wasm` 3/3, and
  `deploy:check` (5 assets, 609.22 KiB, 230.34 KiB gzip). With the owner's
  approval, the encrypted probe was deployed at 10:24 (version `1a93bc18`,
  startup 4 ms).
- `cloudflared` 2026.10.3 ran as a session process (`tunnel --no-autoupdate
  run --token-file`), not a service, at info log level. It chose QUIC by
  default: four connections, bkk04 ×2, sin07, sin14. No connector errors.
- The gateway (debug build) ran in a visible console, so the QR code stayed on
  the host's screen. A wrapper copied only allowlisted status lines to a log,
  never the invite link or QR rows.

**Probe defect found live and fixed.** The first iPhone enrollment
(`5c03-…`) completed the handshake, but the page closed its own socket 11 ms
later with 0 bytes relayed. `probe.mjs` sent the first sample in the same tick
as the last handshake message. `bufferedAmount` still counted that message, so
the backpressure check stopped the probe ("outgoing data is still queued").
The Node tests do not run the page script, so they missed it. The page saves a
pairing only after the first verified echo, so the host kept an orphaned key,
which was removed. Fix: the first sample now waits for the regular one-second
cadence. Local checks were rerun (8/8, build, 3/3, dry run). With the owner's
approval, it was redeployed at 10:35 (version `8ffe3b17`, bundle 12,115 bytes).

**iPhone, iOS 27.0.1, Safari tab (not a Home Screen app).**

- Pairing: scanning the QR code before Access sign-in loaded the page as
  "Not paired"; the Access redirect dropped the `#pair=` fragment. Scanning
  after sign-in worked. The page showed host `a1d1-5d59-ad96-ab17`, and the host
  enrolled **`e22b-7c24-0c77-f505`**.
- Encrypted echo: 185 verified 1 KiB samples over four sessions. Echo RTT was
  18–38 ms, mostly 20–23 ms (Safari reports whole milliseconds). This is a
  transport diagnostic, not desktop latency.
- App switch for about 30 s: iOS dropped the socket within about a second. The
  gateway saw an abrupt receive error with no close frame. The page reported
  "Connection failed" only on return, because its scripts were suspended. The
  probe does not resume by itself. Manual Start reconnected without an invite.
- Lock for about 30 s: the gateway saw no drop; the phone simply stopped
  sending. The synthetic echo target's 60-second read-idle timeout ended the
  session (`1000 RFB connection closed`). The page showed "Connection failed"
  on unlock, and manual Start reconnected. After a phone goes silent, the
  single session slot can stay busy for about 60 s, and up to 20 s more until
  the heartbeat check. A reconnect in that window gets HTTP 409 (relevant to
  T018).
- Reopened tab: Safari brought the page back with the already used `#pair=`
  link; how it was reopened was not identified. The page preferred that link
  over the saved pairing. Nine attempts were rejected with
  `1008 no pairing invite is active`, live evidence that a consumed invite
  cannot be reused. A refresh loaded the saved pairing and worked. The secret
  therefore survives in tab or history state, although the page strips it from
  the address bar.

**Edge 156 on the host, driven through the Claude in Chrome extension.**

- **No plaintext fallback:** raw WebSockets went through Access, the Worker,
  VPC, and the Tunnel. A 1 KiB plaintext sample, like the old probe's, got
  `1008 unsupported E2E handshake`. A 50-byte v1 session header with a random
  body got `1008 E2E handshake failed`. Neither received data, and the gateway
  logged no relay, so its target never opened.
- Pairing from a link the owner pasted on the host enrolled
  **`1458-3de8-ef4a-d683`**: 35 samples at 13.7–15.5 ms.
- **Wrong pinned host key:** a random valid key (`d051-…`) was written into
  `localStorage`; the browser private key was not read. Start got
  `1008 E2E handshake failed`; the page said the host rejected the handshake,
  and no data was exchanged. With the real key restored, 11 samples passed at
  13.0 ms.
- The owner's normal Edge profile also paired twice: `ac28-…` (47 samples)
  and `6809-…`. Both are leftover test keys.

**Usability findings, not fixed:**

- Pasting a `#pair=` link into an already open probe tab is a same-document
  navigation. The page neither rereads nor strips it (there is no `hashchange`
  handling), so the secret stays in the address bar and history. A reload
  reads it.
- A stale invite overrides a saved pairing, and the hint then suggests running
  `--pair` again.
- The gateway's 409 for a busy slot is invisible to the page, which shows the
  generic "Connection failed. Check Access and the private gateway."
- The gateway reports any WebSocket receive error, including an abrupt
  disconnect, as `1009 Invalid or oversized message`, which is misleading.

**Unenrolled browser (2026-10-31, completes T026).** `browsers.txt` held
only `b76d-…` and `1191-…`, written at 01:28 when the owner restarted
`cargo tunnel` and `cargo pair`. The Edge extension profile still had the
`1458-…` pairing saved. The owner signed in to Access in that profile. At
08:40:12 the probe completed the Noise handshake, because XK checks the browser
key only after it, and the gateway then closed with
`1008 browser key is not enrolled`. No sample was exchanged. The owner's
gateway console was not read. An expired invite is covered only by local tests.

**Cleanup.** At 10:59:24, stopped the gateway and `cloudflared`. No process
remains and nothing listens on port 6080. A snapshot just before the stop
showed gateway working set 17,240,064 bytes and cloudflared 41,377,792 bytes;
this is not peak memory. `host.key` and four enrolled keys remain on the host.
The encrypted Worker stays deployed behind Access; with the gateway stopped,
its page cannot connect.

## Production client on the live route (2026-10-31)

Times are +07:00. The owner approved deploying the production client and
switching the host to the production gateway.

- **Deploy.** Wrangler deployed `gateway/cloudflare/worker` with `web/dist`
  under the same Worker name, Access application, and VPC service at 08:41
  (version `9e792e97`, 609.03 KiB). This replaced the Phase 0 probe page. At
  08:55 a toolbar fix was redeployed (version `31ffb4b5`).
- **Host.** The spike gateway (`cargo pair`, started 01:28) was stopped.
  `topvnc-web-gateway` (release build) was started with the ignored
  `gateway.local.toml`, reusing host key `a1d1-…` and both enrolled keys. It
  relays to `topvnc --serve` on `127.0.0.1:5900`, which the owner started with
  a VNC password. `cloudflared` kept running from 01:28.
- **Unenrolled Edge profile.** The production page showed "This browser is not
  enrolled on the host…". The gateway logged
  `session rejected: code=1008 reason=browser key is not enrolled`.
- **iPhone Safari, desktop control (T017, owner-reported).** The connection
  ran through cloudflared, the gateway on 127.0.0.1:6080, and
  `topvnc --serve` on 127.0.0.1:5900. The owner confirmed:
  - the desktop visible and updating;
  - Trackpad pointer movement and tap clicks;
  - two-finger scrolling and pinch zoom;
  - typing with the on-screen keyboard;
  - toolbar keys;
  - disconnect and reconnect with the password.

  Returning from the background was not confirmed. The iOS version was not
  recorded for this run. The owner reported no problems, but asked for an
  Enter key and said the toolbar ran off the screen. The toolbar now scrolls
  inside a screen-width strip with the keyboard and menu buttons pinned. It
  was checked at 390 px in Edge and redeployed; it has not been checked on the
  phone. The gateway logged three relays: 23.9 MB down and 76.5 KB up (closed
  1011, an abrupt browser disconnect), 1.07 MB (1000), and 0.86 MB (1011).
  Three later attempts from unenrolled keys were rejected; they were not from
  this session's Edge profile.
- **Worker CPU (T011).** These are the Workers analytics for the Worker since
  2026-10-30. Only `/ws` upgrades run Worker code. There were 88 invocations:
  0 errors, statuses success, clientDisconnected, and
  responseStreamDisconnected, and none `exceededCpu`. Warm upgrades used about
  1.6-4 ms of CPU at p50. Seven invocations used 13.0-18.7 ms. The highest,
  18.7 ms at 08:42, was the first upgrade after the 08:41 deploy, when a fresh
  isolate fetches and parses the Access signing keys. The documented Workers
  Free limit is 10 ms per invocation. These invocations were not terminated, but
  that is not a guarantee, so the CPU gate is **not passed**. Possible
  mitigations are measuring the key-parse cost and caching parsed keys across
  requests (already done within an isolate). No paid fallback is assumed.

## Worker cold-start CPU (2026-11-01, T027)

Measured locally in Node 22.18.0 on the Ryzen 9 9950X3D host, which is faster
per core than Cloudflare's servers. Each figure is a fresh process, median of
three to five runs.

| Path | Load or import | First key parse | First RS256 check | Total | Warm check |
| --- | --- | --- | --- | --- | --- |
| `topvnc-access` compiled to WebAssembly (as in the Rust Worker) | 3.3 ms | 1.1 ms | 3.0 ms | about 7.3 ms | 0.44 ms |
| JavaScript Worker with WebCrypto | 1.3 ms | 0.6 ms | 0.9 ms | about 2.8 ms | not measured |

Compiling the 625 KB Worker module eagerly with Liftoff took 3.8-4.0 ms, and
V8 compiles lazily, so the first calls pay for the code they reach. This fits
the live pattern: warm upgrades about 2 ms, the first upgrade in a fresh
isolate 13.0-18.7 ms. The owner chose to replace the Worker with plain
JavaScript. Its upload is 14.01 KiB instead of 649.51 KiB. The gateway still
verifies every token with `topvnc-access`, and
`gateway/cloudflare/worker/test/access-vectors.json` (generated and checked by
that crate's tests) makes both implementations accept and reject the same
tokens.

**Live, after the redeploy (2026-11-01).** `npm run deploy` published version
`d2962aca` at about 23:43 +07:00 (Wrangler reported 1 ms Worker startup,
against 4 ms for the Rust Worker). The owner then connected from a desktop
browser on the host through the live route; the iPhone was not paired yet.
Workers analytics (`workersInvocationsAdaptive`, version `d2962aca`):

| UTC | Status | CPU |
| --- | --- | --- |
| 16:44:15 | success | 0.59 ms |
| 16:47:20 | responseStreamDisconnected | 5.87 ms |
| 16:49:28 | responseStreamDisconnected | 4.39 ms |

No errors and no `exceededCpu`. These were the first invocations of the new
version, so the first that verified a token also fetched and parsed the
Access keys in a fresh isolate. The highest was 5.87 ms, against 13.0-18.7 ms
for cold upgrades of the Rust Worker. Three invocations from one browser are
a small sample, so the gate (below) is recorded as passing so far and is
rechecked with the iPhone connections and the soak.

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
| Browser-to-actual-host E2E | Passed live (T026): encrypted echo on iPhone Safari and Edge; plaintext client, wrong pinned host key, reused invite, and unenrolled browser rejected; owner accepted the code-delivery trust limit |
| Owner-only hostname-based Access for workers.dev | Saved; exact owner-email rule and six-hour duration verified; unauthenticated `/` and `/ws` redirect to team login; owner session passed, live wrong-owner session untested |
| Private VPC binding + same-host Tunnel connector | Dedicated service running over healthy QUIC tunnel |
| HTTP/WebSocket upgrade passthrough | Passed basic authenticated Edge echo test on deployed Rust Worker/VPC service |
| Loopback target reachability | Passed for `127.0.0.1:6080`; target stayed loopback-only |
| Wrong-owner/unauthenticated request isolation at the edge | Pending |
| Cold/warm Worker CPU within Free limits | Rust Worker: 88 upgrades, 0 errors, none exceededCpu; warm about 2 ms; 7 cold upgrades 13.0-18.7 ms, above 10 ms. JavaScript Worker (T027, 2026-11-01): first 3 invocations after deploy 0.59-5.87 ms, 0 errors; passing so far, recheck during the iPhone run and soak |
| Two-hour session and slow-link memory behavior | Pending |
| Real iPhone Safari auth/display/input/lifecycle | Echo on iOS 27.0.1 (T026). Desktop control through the production client on 2026-10-31: display, pointer, click, scroll, zoom, on-screen typing, toolbar, disconnect/reconnect owner-confirmed; background return and two hours not yet |

The local tests do not establish any of these live results. No performance
or supported-platform claim is made, and the production refactor stays gated.
