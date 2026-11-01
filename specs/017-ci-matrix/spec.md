# Continuous integration matrix

## Scope

Run the repository gates from `AGENTS.md` automatically on every pull request
and on pushes to `main`, so platform claims in the README can rest on recorded
builds rather than manual runs. This addresses finding 4 of the 2026-11-01
whole-system architecture review.

One GitHub Actions workflow, `.github/workflows/ci.yml`, with three jobs. Every
job installs the toolchain that `rust-toolchain.toml` pins (Rust 1.99.0 with
Clippy and rustfmt, the same pin as the native TLS branch), so a new stable
release cannot fail the run without a code change. Moving to 1.99 required the
`as_chunks` and `try_update` lint fixes that the native TLS branch also carries.


- **Format** (Ubuntu): `cargo fmt --all --check` for the workspace and for the
  separate Cloudflare Worker workspace.
- **Rust** (Windows, macOS, Ubuntu): `cargo clippy --locked --workspace
  --all-targets -- -D warnings` and `cargo test --locked --workspace`. Ubuntu
  installs only the libxcb development packages that `display-info` links;
  winit loads Wayland, X11 and xkbcommon at run time.
- **Browser client and Worker** (Ubuntu): wasm32 Clippy for `topvnc-web` and the
  Worker, a wasm32 release build of the Worker, then `npm ci`, `npm run build`,
  `npm test` and `npm run test:wasm` in `web/`. The wasm-bindgen CLI version is
  read from the root `Cargo.lock` and cached under `.tools`, which
  `web/scripts/build.mjs` already requires.

Out of scope: packaging the Worker with `worker-build` and deploying it, the
`gateway/cloudflare/spike` feasibility workspace, release artifacts, live GUI or
desktop-capture runs, and performance measurement. CI results do not by
themselves make a platform "supported": a passing Linux or macOS build shows the
code compiles and its automated tests pass there, nothing more.

## Acceptance criteria

- The workflow runs on `pull_request`, on `push` to `main`, and on manual dispatch,
  with read-only repository permissions and superseded runs cancelled.
- Every Cargo command uses `--locked`, so a stale lock file fails the run.
- The Rust version comes from `rust-toolchain.toml`, not from the latest stable.
- Clippy warnings fail the run on all three operating systems.
- A matrix failure on one OS does not cancel the others (`fail-fast: false`), so
  each platform's status is visible.
- The browser job fails if the wasm-bindgen CLI and the root `Cargo.lock`
  disagree, through the existing version check in `web/scripts/build.mjs`.
- Each gate the workflow runs passes locally on Windows before the workflow is
  committed; results for macOS and Linux are recorded from the first CI run, not
  assumed.

## Windows reader bug found while setting up CI

The first local run of the gates failed on Windows in
`clients_without_desktop_size_are_disconnected_on_resize`, the timeout that
`specs/012-compact-title` recorded. The cause is a real host bug: when the
server ends a session itself (resize for a client without DesktopSize, a write
error, an exclusive session, or `stop`), it shuts the socket down and waits for
the client's reader thread. On Windows, `shutdown` does not wake a `recv` that
is already blocked, so the reader waited for the peer to close. A peer that
vanished kept its thread and one of the eight connection slots, and
`ClientDisconnected` never arrived. A standalone probe confirmed that a blocked
read is still blocked two seconds after `shutdown(Both)`.

Fix: the session registry, the session's `NativeStream` and every clone of it
share one socket handle (`Arc<TcpStream>`), and every server-side close goes
through `shut_down_client` or `NativeStream::interrupt_reads`, which shut the
socket down and then call `topvnc_net::interrupt_reads`. On Windows that cancels
the handle's pending I/O (`CancelIoEx`); elsewhere it does nothing. The one
unsafe call lives in `crates/topvnc-net`, so `src/lib.rs` keeps
`#![forbid(unsafe_code)]`. A read that starts just as the socket shuts down can
miss one cancellation, so the session repeats it for up to about 200 ms until
the reader thread has finished. The probe also confirmed that bytes received
before the shutdown are still read in order and that a read started afterwards
returns at once.

The native TLS work (`specs/016-native-tls`) fixed the same bug independently
with a 100 ms receive timeout that let the reader poll a stop flag. That was
replaced when the branches merged. Winsock documents a connection as
indeterminate after a blocking receive times out (`SO_RCVTIMEO`), and an idle
session timed out ten times a second. A cancellation happens only when the
session is already closing, so it cannot lose input the session still needs.
Sharing the handle is what lets the cancellation reach the TLS reader, which
reads through a clone of the session stream.

Regression tests: the resize test above,
`stopping_the_server_disconnects_a_client_that_keeps_its_socket_open`, and three
`topvnc-net` tests. With `interrupt_reads` temporarily made a no-op, both server
tests fail on Windows; with it, they pass.

## Validation

On Windows 11 with the pinned Rust 1.99.0 and Node 22.18, every step the
workflow runs passes locally:

- `cargo fmt --all --check` for the workspace and the Worker.
- `cargo clippy --locked --workspace --all-targets -- -D warnings`.
- `cargo test --locked --workspace`: 252 passed, 7 ignored, 0 failed. Before the
  reader fix (on Rust 1.95), 247 passed and 1 failed.
- wasm32 Clippy for `topvnc-web` and the Worker, and the Worker's wasm32 release
  build.
- `npm run build`, `npm test` (9 passed) and `npm run test:wasm` (2 passed) in
  `web/`.

### First GitHub run

Run 41318680312 on commit `14a7b0e` (draft PR #7, 2026-11-01):

- Passed: Format, Rust (windows-latest) and Browser client and Worker. Every
  action version resolved, and the pinned toolchain installed on all runners.
- Rust (ubuntu-latest): the Linux package step passed; Clippy failed to compile
  the `hosted_server_reports_unsupported_platforms_and_stops` test in
  `src/main.rs`, which still passed `display: None` after `ServeRequest.display`
  became a `DisplaySelection`.
- Rust (macos-latest): Clippy failed with dead code in `src/helpers.rs`. The
  text of `Action::Copy` is read only by the Windows helper runner.

Neither error came from this feature: both are in code compiled only on
non-Windows targets, which no automated build had compiled before. Tests did not
run on either system because Clippy runs first. Both errors are fixed in the
next commit, whose run is recorded below.
