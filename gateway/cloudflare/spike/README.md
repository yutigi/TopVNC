# Phase 0: Rust gateway through a free workers.dev URL

> Superseded for desktop use by the production client and gateway:
> `crates/topvnc-web`, `crates/topvnc-web-gateway`, `gateway/cloudflare/worker`,
> and [docs/cloudflare-gateway.md](../../../docs/cloudflare-gateway.md). This
> harness stays as the synthetic route probe.

This is an isolated feasibility harness, not the production TopVNC browser
client. The root native package and session implementation are unchanged.
The basic Workers VPC/WebSocket route passed in Edge. The encrypted probe passed
encrypted echo live in iPhone Safari and Edge; the remaining live checks, and
the two-hour and resource gates, are listed in the feature's research.md.

**Desktop deployment is blocked.** Current main requires browser-to-actual-host
end-to-end encryption before Cloudflare carries RFB. The harness now has that
channel (below), tested locally only. The executable still accepts only
`--synthetic-echo`, replaces the configured TCP target with its own loopback
echo server, and never connects to a desktop. The browser sends generated
samples and has no password field. See the feature's research.md for the live
deployment state.

The harness contains a native Rust WebSocket-to-loopback-TCP relay, shared
RS256/JWKS policy, a shared Noise E2E crate (`e2e/`) with WASM bindings
(`browser/`), a workers-rs upgrade forwarder, a synthetic browser echo probe,
and a cloudflared launcher (`tunnel/`). The original noVNC password-only adapter and tests remain as local
reference; noVNC is no longer imported into the browser bundle. This is not the
planned Rust/WASM TopVNC client.

## Browser-to-host encryption and its trust limit

- **Protocol**: Noise `XK_25519_ChaChaPoly_BLAKE2s` through the `snow` crate's
  pure-Rust resolver, in the gateway and in browser WASM. The gateway on the
  host decrypts; Cloudflare's Worker, VPC service, and Tunnel carry only
  ciphertext. One Noise message travels in each binary WebSocket message.
- **Host identity**: `--init-host-key` creates the host's static key once and
  never replaces it. The browser pins the public key from a pairing invite read
  off the host's own screen, never from Cloudflare.
- **Browser identity**: the gateway accepts only enrolled browser keys (at most
  eight, in `browser_keys_file`). `--pair` prints a QR code containing the host
  public key and a one-time 256-bit secret, valid for ten minutes. Scanning it
  opens `#pair=...`, a fragment browsers do not send in requests. The browser
  makes a new key and enrolls it with Noise `XKpsk3`. Failed guesses do not
  consume the invite; success does.
- **Failure**: any changed host key, unknown browser key, wrong or expired
  secret, malformed or timed-out handshake, or altered, replayed, reordered, or
  dropped message closes the session with a reason. The target opens only after
  the handshake; there is no plaintext fallback.
- **Trust limit (accepted by the owner on 2026-10-30)**: Cloudflare serves
  the page's JS/WASM. Cloudflare, or anyone who controls the Cloudflare account
  or Worker deployment, can serve modified code that reads the session and the
  browser's stored key. E2E protects against passive observation at the edge,
  logs, Worker/VPC/Tunnel misconfiguration, a stolen Access JWT or tunnel token,
  and host impersonation. It does not protect against substituted client code.
  Keep account 2FA on and sign Wrangler out when not deploying.
- **Browser storage**: the pinned host key and browser private key live in
  `localStorage` for the workers.dev origin. Safari may delete script-written
  storage after seven days without a visit, unless the app was added to the
  Home Screen; the client then refuses to connect until you pair again.
- **Rotation and revocation**: delete `host_key_file` and run `--init-host-key`
  to rotate (every browser must pair again). Delete a line from
  `browser_keys_file` and restart the gateway to revoke a browser.

## Local checks

From this directory:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p topvnc-spike-worker -p topvnc-spike-browser --target wasm32-unknown-unknown -- -D warnings
```

Install the pinned packager and bindings generator locally, then build from
`worker/`. The `wasm-bindgen-cli` version must equal `wasm-bindgen` in
Cargo.lock:

```powershell
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.7 --locked --root .tools
cargo install wasm-bindgen-cli --version 0.2.129 --locked --root .tools
Set-Location worker
npm ci
npm test
npm run build
npm run test:wasm
npx wrangler deploy --dry-run --config wrangler.example.toml --outdir build/dry-run
```

Keep release `strip = "debuginfo"`: `strip = true` removes metadata required
by the pinned wasm-bindgen packager. All generated build/tool files are ignored.

## Account setup before deployment

1. Run `npm run login` from `worker/`. Wrangler OAuth is sufficient for
   Workers/VPC commands but may not have permission to inspect billing or
   configure Access; HTTP 403 on those APIs does not prove the services are
   unavailable.
2. Confirm **Workers Free** in the Cloudflare dashboard. Do not select a paid
   upgrade. Use Zero Trust's free option for one owner.
3. Protect the exact `topvnc-connection-spike.<subdomain>.workers.dev` hostname
   with a **hostname-based Access application**, an Allow rule for the owner's
   exact email, and no bypass policy. Keep previews disabled. Authenticate
   interactively before opening the socket. Worker-level Access policies
   currently reject WebSocket upgrades; do not substitute the Worker-level
   protection option. [Cloudflare Access for Workers](https://developers.cloudflare.com/workers/configuration/cloudflare-access/).
4. Copy the Access team URL and application AUD tag into the local configs.
   These identify the expected issuer/application; they are not login secrets.
   Keep owner/account-specific configs ignored rather than committing them.
5. Create a **private Tunnel for Workers VPC** in the same account. No public
   Tunnel hostname or owned domain is needed by the proposed private route.
   Run its same-host cloudflared connector using QUIC, with outbound UDP 7844
   available. Keep the connector token in a protected, ignored local file;
   do not put it in browser assets or logs.
   [Workers VPC Tunnel setup](https://developers.cloudflare.com/workers-vpc/configuration/tunnel/).
6. With the Tunnel ID, create a fixed HTTP VPC service (loopback reachability
   remains a live test):

   ```powershell
   npx wrangler vpc service create topvnc-spike --type http --tunnel-id YOUR_TUNNEL_ID --ipv4 127.0.0.1 --http-port 6080
   ```

7. Copy `host.example.toml` to `host.local.toml` and
   `worker/wrangler.example.toml` to `worker/wrangler.local.toml`. Fill the
   account, origin, Access issuer/AUD, owner email, and VPC service ID. An
   older `host.local.toml` needs the two new key-file settings.
8. From this directory, create the host identity once. Then start the
   synthetic-only relay with a pairing invite. Do not start TopVNC:

   ```powershell
   cargo run -p topvnc-spike-host -- --init-host-key host.local.toml
   cargo pair
   ```

   Run `cargo gateway` instead once the browser is enrolled. Both are aliases
   for `cargo run -p topvnc-spike-host -- --synthetic-echo [--pair] host.local.toml`,
   defined in `.cargo/config.toml` here and at the repository root, so they
   work from either directory. Show the QR code only on the
   host's own screen; use a dark console background if the camera does not
   detect it.

9. From `worker/`, run `npm run deploy:check`, then deploy only after the Free
   account and owner-only Access configuration are confirmed. The deploy
   script uses the ignored local config, not the example's placeholders.
10. Open the workers.dev URL and sign in through Access. Then scan the QR code
    (an Access redirect can drop the `#pair=` fragment; reload if you pasted
    the link into an already open probe tab) and compare the
    displayed host fingerprint with the console. Start the encrypted test. It
    verifies one generated 1 KiB sample at a time, once per second, handling
    arbitrary transport chunks with a five-second deadline. The first verified
    echo saves the pairing. Echo RTT is a transport diagnostic, not a desktop
    latency measurement.

## Testing from an iPhone

This repeats steps 8 and 10 for one test session after setup and deployment.
It checks only the encrypted echo; the iPhone does not see or control a
desktop. Run each long-lived command in its own console window that you watch
yourself, never in a shell whose output is captured or logged: the QR code and
invite link carry a one-time secret.

1. From the repository root or this directory, start the connector:

   ```powershell
   cargo tunnel
   ```

   The `tunnel/` launcher checks that the protected token file from step 5
   exists, without reading it, and runs
   `cloudflared tunnel --no-autoupdate run --token-file tunnel-token.txt`. Set
   `CLOUDFLARED` to the full path if `cloudflared` is not on `PATH`. Ctrl+C
   reaches cloudflared directly; the launcher waits for it to exit. Wait for
   `Registered tunnel connection` lines. Without them the page cannot reach the
   gateway.

2. In a second window, from the repository root or this directory, start the
   gateway with a pairing invite:

   ```powershell
   cargo pair
   ```

   The console prints `Host key fingerprint: …`, the QR code, the invite link,
   and `Phase 0 synthetic gateway listening on 127.0.0.1:6080 …`. Maximize the
   window so the QR code does not wrap. The invite works once, for ten minutes;
   stop and run `cargo pair` again for a new one.

3. In iPhone Safari, open the `app_origin` URL and finish the Access sign-in
   **before** scanning. The Access redirect drops the `#pair=` fragment, and the
   page then says "Not paired".
4. Scan the QR code with the Camera app and open the link. The page shows
   `Pairing invite for host …`. A scanned invite takes priority over a saved
   pairing, so **Forget pairing** is not needed first. Compare the host
   fingerprint with the console, then press **Start encrypted test**. The first
   verified echo saves the pairing, and the console prints
   `enrolled browser key …`, which should match the browser fingerprint on the
   page.
5. For later runs, start the gateway with `cargo gateway`. The page shows
   `Paired with host …`; press **Start encrypted test**.
6. When finished, press Ctrl+C in both windows. Nothing should be listening on
   port 6080 afterwards.

Pairing again leaves the old browser key enrolled, and the limit is eight. To
remove it, stop the gateway, delete its line from `browsers.txt`, and restart
(see "Rotation and revocation" above).

Behavior seen in iOS 27 Safari on 2026-10-30:

- Switching apps or locking the phone ends the session, and the probe does not
  reconnect by itself; press **Start encrypted test** again. After a lock, the
  gateway may not see the drop, so its single session slot stays busy for about
  60–80 s (60 s of silence plus up to 20 s until the heartbeat check). A
  reconnect in that window gets HTTP 409, which the page shows only as
  `Connection failed. Check Access and the private gateway.`
- Safari can bring back the page with an already used `#pair=` link from tab or
  history state. That invite overrides the saved pairing, and the page shows
  `Disconnected (1008). The pairing invite expired or was already used…`. Open
  the plain URL instead of running `cargo pair` again.
- The gateway logs any abrupt disconnect, such as an app switch, as
  `1009 Invalid or oversized message`. It is not a size problem.

For the two-hour run, set Auto-Lock to Never, keep the phone on a charger, and
keep Safari in front. The session ends at `max_session_seconds` (7200 in the
example config), or earlier if the Access session expires. Record the
gateway's `relay closed: code=… upstream_bytes=…` line and the page's last
status line and recent round-trip times in research.md.

## What to prove before production work

- VPC HTTP `fetch()` preserves WebSocket upgrades and binary data in both
  directions; Rust SDK compilation alone is not evidence of runtime support.
- The private connector can reach the loopback listener. The binary requires a
  loopback listener and generates its own loopback echo target. If loopback fails, investigate and
  record an explicit same-host/firewall design rather than opening the port.
- Access protects assets and `/ws`, rejects other users, and recovers from expiry.
- Cold/warm JWT verification, handshake, and a two-hour synthetic stream fit
  Workers Free. Verify that runtime passthrough does not invoke a message loop.
- Safari login, pairing, encrypted echo, background/foreground, and reconnect
  work on the actual device. Desktop authentication/display/input require the
  production client and a separate later test. The harness is not a claim of
  product support.

Write actual measurements and the selected architecture to
`specs/015-cloudflare-web-client/research.md`. If the route fails, a separately
measured Rust VPC raw-TCP relay may be investigated under the same constraints.
Do not upgrade plans, buy a domain, or present a temporary Quick Tunnel as the
stable deployment.

## Resource and authentication bounds

- JWT: 16 KiB; JWKS: 64 KiB, at most eight RSA keys of 2048–4096 bits.
- Verify RS256 signatures, issuer, audience, expiry, optional not-before, and
  owner email; reject unknown critical JWT extensions.
- Cache keys for five minutes; permit refresh attempts at most every ten
  seconds. Fail closed on unavailable/invalid keys.
- Native relay: eight concurrent HTTP upgrades, one active session, 16 KiB
  plaintext per E2E message (16 KiB + 16-byte tag maximum message/fragment
  aggregate), fixed-size handshake messages within ten seconds, bounded
  WebSocket write buffer.
- Await each write with a five-second deadline, capped by session expiry;
  heartbeat every 20 seconds and a two-hour maximum session lifetime.
- No destination from a query/message, no redirect forwarding, no text payloads,
  no persistent passwords or JWTs, and no payload logging. The gateway logs
  browser-key fingerprints at enrollment, close reasons, and byte counts.
