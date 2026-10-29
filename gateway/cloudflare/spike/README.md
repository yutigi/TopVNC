# Phase 0: Rust gateway through a free workers.dev URL

This is an isolated feasibility harness, not the production TopVNC browser
client. The root native package and session implementation are unchanged.
The live Workers VPC/WebSocket and iPhone gates have **not** passed.

**Desktop deployment is blocked.** Current main requires browser-to-actual-host
end-to-end encryption before Cloudflare carries RFB. This harness does not
provide it. Its executable therefore only accepts `--synthetic-echo`, replaces
the configured TCP target with its own loopback echo server, and never connects
to a desktop. The browser sends generated samples and has no password field.
Owner-only Access setup does not remove the E2E gate. See the feature's
research.md for the current synthetic deployment and temporary connector state.

The harness contains a native Rust WebSocket-to-loopback-TCP relay, shared
RS256/JWKS policy, a workers-rs upgrade forwarder, and a synthetic browser echo
probe. The original noVNC password-only adapter and tests remain as local
reference; noVNC is no longer imported into the browser bundle. This is not the
planned Rust/WASM TopVNC client.

## Local checks

From this directory:

```powershell
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo check -p topvnc-spike-worker --target wasm32-unknown-unknown
```

Install the packager locally, then build from `worker/`:

```powershell
rustup target add wasm32-unknown-unknown
cargo install worker-build --version 0.8.7 --locked --root .tools
Set-Location worker
npm ci
npm test
npm run build
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
   account, origin, Access issuer/AUD, owner email, and VPC service ID.
8. From this directory, start the synthetic-only relay. Do not start TopVNC:

   ```powershell
   cargo run -p topvnc-spike-host -- --synthetic-echo host.local.toml
   ```

9. From `worker/`, run `npm run deploy:check`, then deploy only after the Free
   account and owner-only Access configuration are confirmed. The deploy
   script uses the ignored local config, not the example's placeholders.
10. Open the workers.dev URL, sign in through Access, and start the connection
    test. It verifies one generated 1 KiB sample at a time, once per second,
    handling arbitrary transport chunks with a five-second deadline. Echo RTT
    is a transport diagnostic, not a desktop latency measurement. Cloudflare
    can read these non-sensitive generated samples.

## What to prove before production work

- VPC HTTP `fetch()` preserves WebSocket upgrades and binary data in both
  directions; Rust SDK compilation alone is not evidence of runtime support.
- The private connector can reach the loopback listener. The binary requires a
  loopback listener and generates its own loopback echo target. If loopback fails, investigate and
  record an explicit same-host/firewall design rather than opening the port.
- Access protects assets and `/ws`, rejects other users, and recovers from expiry.
- Cold/warm JWT verification, handshake, and a two-hour synthetic stream fit
  Workers Free. Verify that runtime passthrough does not invoke a message loop.
- Safari login, echo, background/foreground, and reconnect work on the actual
  device. Desktop authentication/display/input require an E2E design and a
  separate later test. The harness is not a claim of product support.

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
- Native relay: eight concurrent handshakes, one active session, 64 KiB maximum
  message/fragment aggregate, 16 KiB TCP chunks, bounded WebSocket write buffer.
- Await each write with a five-second deadline, capped by session expiry;
  heartbeat every 20 seconds and a two-hour maximum session lifetime.
- No destination from a query/message, no redirect forwarding, no text payloads,
  no persistent passwords or JWTs, and no payload logging.
