# Implementation plan

Spec: [spec.md](spec.md), approved by the owner on 2026-11-04. Build Run
(US1, US2) first and Setup (US3) second. Each phase ends with `cargo fmt --all`,
`cargo test --workspace`, and `cargo clippy --workspace --all-targets`.

## Phase 1: Run view

### 1. Supervised gateway mode (`crates/topvnc-web-gateway`)

`topvnc-web-gateway --supervised <gateway.toml>` keeps the console modes
unchanged and adds a private line protocol over the child's pipes:

| Direction | Line | Meaning |
| --- | --- | --- |
| stdin | `pair` | Open a new one-time invite (replaces an earlier one). |
| stdin | `cancel-pair` | Drop the open invite, if any. |
| stdin | end of file | Shut down gracefully. The parent closing the pipe, or dying, stops the gateway. |
| stdout | `fingerprint <fp>` | Host key fingerprint, once at start. |
| stdout | `ready <addr>` | Listening; the gateway is usable. |
| stdout | `pair <seconds> <link>` | The invite link. Only TopVNC reads it; it never goes to stderr. |
| stdout | `pair-error <reason>` | The invite could not be opened (for example the list is full). |

stderr stays the human log (`relay opened`, `session rejected`,
`enrolled browser key …`). In supervised mode the gateway prints no QR code
and no link. `Gateway::cancel_pairing` is added to the library. Pairing at
runtime means a phone session is not dropped to pair another browser.

An integration test runs the real binary with a scratch config and host key:
it reads `fingerprint` and `ready`, sends `pair`, checks the link's origin and
`#pair=` part, checks that stderr never contains `#pair=`, sends
`cancel-pair`, closes stdin, and expects a clean exit within a few seconds.

### 2. Web access module in `topvnc` (`src/web_access/`)

No platform UI code; the landing window drives it.

- `paths.rs`: find the repository root (ancestors of the executable, then
  `CARGO_MANIFEST_DIR`) by `gateway/cloudflare/gateway.example.toml`; the
  config, token-file, and host-key paths; the gateway binary next to the
  `topvnc` executable; cloudflared from `CLOUDFLARED` or `PATH`.
- `config.rs`: parse `gateway.local.toml` with the gateway's field names
  (`deny_unknown_fields`) and check what `Gateway::new` checks: loopback
  `listen` and `rfb_target`, a nonzero RFB port, a 1-7200 s session cap,
  `topvnc_access::validate_origin`, and `AccessPolicy::new`. Then check that
  the host key and token files exist (never reading the token). Each failure
  names the Setup step that fixes it. `topvnc` gains `topvnc-access`, `serde`,
  and `toml`, which are already in the workspace. It cannot depend on the
  gateway crate, which dev-depends on `topvnc`.
- `lines.rs`: pure line rules, unit-tested: gateway stdout protocol,
  `enrolled browser key`, cloudflared's `Registered tunnel connection`,
  `Unregistered tunnel connection`, `Connection terminated`, and
  `Lost connection with the edge` (found in cloudflared 2026.10.3; tracked
  per `connIndex`), and redaction of anything holding `#pair=`.
- `process.rs`: one supervised child. No console window
  (`CREATE_NO_WINDOW`), piped stdin, stdout, and stderr. Reader threads
  classify lines themselves and store them in a shared, bounded ring
  (64 lines of at most 512 characters) and a small event queue, so a child
  never blocks on a full pipe while the viewer window is in a session. Output
  is never written to disk.
- `job.rs` (Windows): one Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`
  holds every child. If TopVNC dies, Windows ends the gateway and cloudflared.
  Other platforms rely on the gateway's stdin end-of-file and kill
  cloudflared on drop; they are not claimed tested.
- `runtime.rs`: the `WebRuntime` state machine with three rows (Server,
  Tunnel, Gateway), each Stopped, Starting, Ready, Stopping, or Failed with a
  reason. It starts in order (server, then cloudflared once the server is
  Serving, then the gateway once the tunnel registers), with start limits of
  20 s, 45 s, and 15 s, and stops in reverse (close the gateway's stdin and
  wait up to 3 s before killing it, then kill cloudflared, then stop the
  server only if the WEB tab started it). Children are created through a
  small spawner trait so tests use fakes. The server is not owned here: each
  poll takes the hosted server's view and returns a start or stop request for
  `main.rs` to apply.
  - Reusing a server already started from the Server tab requires its address
    to equal `rfb_target`, a password, and plain RFB. Otherwise Start refuses
    and changes nothing.
  - The WEB server uses plain RFB on `rfb_target` with the typed password and
    the Server tab's display, size, mouse, priority, and performance settings.
    It never uses All networks or `--allow-insecure`.
- `pairing.rs`: turns a `pair` line into a QR module matrix (the `qrcode`
  crate, no default features), the fingerprint, and a deadline. The link
  string is dropped once encoded. The matrix is cleared on enrollment,
  expiry, Cancel, Stop, or a gateway exit.

### 3. WEB tab (`src/ui.rs`, `src/main.rs`)

- `Tab::Web` becomes a third top tab, `WEB`, after `SERVER`. The `...` menu
  moves right.
- Run layout on the 800 × 640 canvas: the VNC password field (shared with the
  Server tab's password and never saved), three status rows with a colored
  light, state, and detail, the recent output of the selected or failed row,
  and Start/Stop and Pair a browser/Cancel buttons. While pairing, the QR code
  (at least 4 px per module with a quiet zone), the fingerprint, the minutes
  left, and the sign-in-first reminder fill the right side.
- `HostedServer` records whether its server is plain RFB, for the reuse rule.
  `WebRuntime` lives beside `HostedServer` in `main()`, so it survives viewer
  sessions, and its `Drop` stops the children.
- The window only reads state from `WebRuntime`; drawing stays in `ui.rs`.

### 4. Docs and validation

Update `docs/cloudflare-gateway.md` (Each session, Pair your phone,
Troubleshooting) with the WEB tab as an alternative to the consoles. Record
local results and the remaining live checks in `validation.md`.

## Phase 2: Setup view

Planned after Phase 1 is reviewed, using the same process module for
Wrangler, npm, and cargo commands (with time limits and Cancel), atomic
config writes that keep the example files' comments, Windows ACLs set
through the Win32 security API (not `icacls` text parsing) and verified after
writing, a masked token field written straight to the file, a confirmation
before `vpc service create` and `deploy`, and the unauthenticated app-URL
check after deploy (tested against a local fake server). Details go in an
update to this plan before Phase 2 starts.

## Risks

- cloudflared's log wording is not a stable interface. The rules are
  unit-tested against the 2026.10.3 strings, and an unknown format leaves the
  row at Starting until its time limit, which then says so.
- Killing cloudflared is not a graceful tunnel shutdown. Cloudflare treats
  it as a lost connection; the next start re-registers.
- The software-drawn font has only uppercase letters, digits, and some
  punctuation, so status text is uppercase and short.
