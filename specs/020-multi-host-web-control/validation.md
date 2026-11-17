# Local validation — 2026-11-07

Windows workspace, no deployment or commit.

| Check | Result |
| --- | --- |
| `npm --prefix web test` | 47 passed: pairing isolation/migration, metadata bounds, invite targeting and changed pins, lifecycle fencing, existing gestures/text/input/view behavior |
| `npm --prefix gateway/cloudflare/worker test` | 22 passed: shared Rust JWT fixtures, two private bindings, owner-protected metadata, rejected destinations/IDs/auth, invalid registry, offline-host isolation, legacy route |
| `npm --prefix web run build` | Passed with pinned wasm-bindgen and esbuild |
| `npm --prefix web run test:wasm` | 8 passed against the built WASM and real session module |
| `npm --prefix web run test:integration` (named `test:browser` until the merge with main, which uses that name for its layout check) | Passed in headless Chrome 156.0.8078.80 on Windows, actual built page with a controlled session Worker |
| `cargo fmt --all` | Passed |
| `cargo test --workspace` | Passed, including gateway host-ID/link tests, supervised QR protocol, and native WEB setup validation; existing ignored benchmark retained |
| `cargo clippy --workspace --all-targets` | Passed |
| `cargo check -p topvnc-web-gateway --features test-fixtures --example local_web` | Passed; existing single-host loopback harness still builds |
| Wrangler `unstable_readConfig` | Both checked-in example configurations parsed with the installed pinned Wrangler; shared example has two VPC bindings and all three Worker route patterns |
| `git diff --check` | Passed |

The browser check exercises the selector during a connected session with a
latched Ctrl key, password clearing, release/close messages, old canvas
detachment, explicit reconnect, late verified/connected/resize messages,
host-scoped enrollment, wrong-host links, asynchronous disconnect diagnostics,
and the legacy `/ws` connection path. It uses synthetic Worker messages;
protocol encryption and resource release are covered separately by WASM,
Rust protocol, session-input, and gateway tests.

The initial Cargo test encountered stale cached protocol artifacts referring
to an enum variant absent from source. `cargo clean -p topvnc-rfb` followed
by the workspace test rebuilt that crate and passed, without protocol edits.
The browser test required execution outside the restricted process sandbox
so Chromium subprocesses and DevTools could communicate. It used a temporary
profile and a loopback-only server, removed afterward.

## Review follow-up — 2026-11-17

Session worker start-up per Connect, headless Chrome 157.0.8103.51 on
Windows, built `topvnc_web_bg.wasm` (412,352 bytes). The server mimicked
Workers assets: ETag with `must-revalidate`, so a worker's WASM fetch is a
304 revalidation. Each value is a median of 15–20 interleaved starts after
warm-up, from `new Worker` to the worker reporting WASM ready.

| Link | Worker only | Worker fetches and compiles WASM (before) | Worker instantiates the page's module (after) |
| --- | --- | --- | --- |
| Loopback | 4.5–6.0 ms | 6.3–8.2 ms | 4.6–6.3 ms |
| Simulated 40 ms RTT | 92.4–92.8 ms | 139.6–139.9 ms | 92.5–93.1 ms |

Passing the compiled module removes one round trip per Connect on a real
link and about 2 ms of compilation locally. A separate check loaded the
built `session-worker.js` under the production CSP and posted it the
module: it initialized and opened its WebSocket without fetching the WASM
again. Not measured: iOS Safari or a live Cloudflare link.

The first CI run of `test:integration` failed on Linux: navigation waits
such as `!document.querySelector('#connect')?.disabled` passed while the new
page was still loading. Under 6x CPU throttling the old test failed 3 of 3
runs with the same error; the corrected waits passed 3 of 3, and unthrottled
5 of 5.

## Remaining live verification

- Deploy only when requested, with the shared Access policy covering the
  entire website, and route two independent private tunnels/services.
- Pair both hosts and switch repeatedly on desktop and iPhone Safari; verify
  held keys/buttons release on the actual desktops and old pixels never show.
- Exercise an offline tunnel/gateway, expired Access login, wrong owner,
  reconnect, and migration of an existing browser's legacy pairing.
- Measure cold Worker CPU and session pacing before performance claims.

No live Cloudflare route, physical remote machine, mobile-device run, new
platform support, or performance improvement is claimed by this change.
