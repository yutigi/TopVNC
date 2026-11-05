# Server tab and browser access at the same time

**Feature Branch**: `feat/server-and-web-together`

**Created**: 2026-11-05

**Input**: Owner: "why i can't use hosting server and web on the same time?" then "do real fix".

## Scope

Before this feature the landing window had one desktop server, and the WEB tab
could only use it when the Server tab served plain RFB with a password on the
gateway's loopback `rfb_target`. A normal Server tab setup (TLS on the local
network) made the WEB tab refuse with "Stop that server first". Running a
second server was not an option: Windows lets a process duplicate a display
only once, and a second capture would double CPU and GPU use.

One `VncServer` now accepts viewers on several listeners. Each listener has its
own address, transport security (TLS or plain), and password; all of them share
one framebuffer, one capture, one input path, and one connection limit. Hosts
add and remove listeners while the server runs through a `ServerListeners`
handle, which the platform servers report with `ServerNotice::Listeners` just
before `Serving`.

In the landing window, the Server tab and the WEB tab each hold their own
listener on that shared server:

- Whichever tab starts first starts the server with its listener. The other tab
  adds its listener to the running server, or waits until it has started.
- Stopping one tab closes only its listener and ends only the viewers that
  arrived on it. The server stops when neither tab has a listener.
- When both tabs ask for the same address, they share one listener if their
  viewers would connect the same way (same TLS choice and password); otherwise
  the second tab is refused with a message naming the address. An "All
  networks" listener and a loopback listener on the same port can coexist.
- The display, size, mouse mode, priority, and Ultra mode are server-wide. A tab
  that joins a running server keeps the choices it started with, and the Server
  tab says so when it joins.
- The Server tab shows only its own listener. While only browser access serves,
  the Server tab shows Stopped and can start its own listener.

Out of scope: more than two listeners from the UI, per-listener display or size,
changing a running listener's settings, and live validation with a browser
session (the WEB tab has not carried one yet; see feature 018).

## Acceptance criteria

- Library: viewers on two listeners each authenticate only with their own
  listener's password and see the same framebuffer. Removing a listener ends
  only its viewers and refuses new connections on its address. Listeners need a
  password or explicit `allow_insecure`. Stopping or dropping the server frees
  every port even while a handle to the listener set still exists, and no
  listener can be added afterwards.
- Landing window, with a fake server thread and real listeners: browser access
  joins a running Server tab server and leaves it running when it stops; the
  Server tab joins and leaves a browser-access server without stopping it; one
  address is shared only with matching security; a tab that joins while the
  server starts waits for its listener; the first listener closes if its tab
  stops during start; a listener that cannot open is reported and the server
  keeps running; a stopping server refuses new listeners until it has stopped.
- The WEB runtime always asks for its own listener, needs a password, and waits
  for an earlier listener of its own to close before it starts again.
- `cargo fmt --all`, `cargo test --workspace`, and `cargo clippy --workspace
  --all-targets` pass on Windows. macOS changes are one notice line in the
  server and are not compiled or run by this change's checks.

## Validation

Recorded 2026-11-05, Windows only, automated:

- `cargo test --workspace` and `cargo clippy --workspace --all-targets`.
- A probe on Windows 11 confirmed that binding `0.0.0.0:P` and `127.0.0.1:P` in
  one process succeeds in either order. Rust's standard library sets
  `SO_REUSEADDR` on Unix, which allows the same split on macOS; this was not
  checked on a Mac.

Not yet done: a live run with both tabs serving a native viewer and a browser at
once, and any latency or CPU measurement. No performance change is claimed.
