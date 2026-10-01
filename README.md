# TopVNC

A VNC viewer written in Rust, built toward responsive remote gaming with low input latency and smooth frame presentation.

TopVNC can also serve a Windows or macOS display over RFB 3.8. The reusable server protocol is in the library; the Windows and macOS executables capture the desktop and inject remote keyboard and mouse input. Linux hosting is not implemented yet. See [`specs/004-vnc-server/spec.md`](specs/004-vnc-server/spec.md) for protocol and Windows limits, and [`specs/005-macos-server/spec.md`](specs/005-macos-server/spec.md) for the macOS host.

![TopVNC connection screen with server, authentication, encoding, window size, frame rate, and scaling controls](docs/images/topvnc-connection.png)

*TopVNC running on Windows, with connection and display settings in one window.*

**Early development:** the native client has been compiled and tested on Windows and connected to a live macOS VNC server. The Windows server mode compiles and links, and its platform-neutral logic is tested, but live desktop capture and input injection have not yet been verified on Windows (an earlier attempt outside an interactive desktop was denied with `0x80070005`). The macOS server mode builds and its platform-neutral logic is tested on macOS, but live capture and input injection have not yet been validated. Full remote control and macOS unlocking remain under live validation. macOS and Linux client builds are untested; Linux hosting is not implemented. Performance goals have not yet been established by end-to-end benchmarks.

[Quick start](#quick-start) · [Controls](#controls-and-display-settings) · [Security](#security-and-saved-settings) · [Development](#development) · [Roadmap](#roadmap)

## Features

- **Native connection UI** with a masked password field, input validation, and connection errors shown in the window.
- **Raw and Zlib encodings**, 32-bit true color, and incremental framebuffer updates.
- **Keyboard and mouse input** for interacting with the remote desktop.
- **Adjustable display** with fit-to-window or native pixels, smooth or sharp scaling, and 30, 60, or 120 FPS presentation limits.
- **In-session settings** accessible through **F8**, including disconnect and a resizable on-screen settings button.
- **Saved connection details** with platform-specific password storage.
- **Experimental Windows and macOS server modes**, started from the app's Server tab or with `--serve`, with display capture, resolution-change handling, remote keyboard/mouse input, clipboard sync, and password authentication; live validation is pending.

The protocol implementation handles RFB 3.3, 3.7, and 3.8. Apple's `RFB 003.889` banner is handled through a standard RFB 3.8 fallback; Apple-specific authentication is not implemented.

## Quick start

You need a stable Rust toolchain with Cargo and native build tools for your platform. Windows is the currently tested client platform. To connect, you also need a reachable VNC server configured for standard VNC password authentication.

```sh
git clone https://github.com/yutigi/TopVNC.git
cd TopVNC
cargo run --release
```

1. Enter the server address and port (usually `5900`). Use brackets around IPv6 addresses, such as `[::1]`.
2. Enter the server's VNC password.
3. Choose Raw or Zlib, the initial window size, presentation rate, and scaling mode.
4. Click **Connect**. Press **F8** during the session to adjust display settings or disconnect.

> **Transport security:** TopVNC currently uses unencrypted TCP. VNC password authentication does not encrypt the framebuffer or input traffic. Use an isolated network or a separately secured tunnel for sensitive sessions. See [Security and saved settings](#security-and-saved-settings).

### Command-line options

Arguments prefill the connection form; they do not bypass it.

```sh
cargo run --release -- 127.0.0.1:5900
cargo run --release -- 127.0.0.1:5900 --window 1280x720
```

| Argument | Effect |
| --- | --- |
| `HOST:PORT` | Prefill the server address and port. |
| `--fit` | Fit the remote image to the viewer window. |
| `--native-size` | Show native pixels; center and crop if the image is larger than the window. |
| `--window WIDTHxHEIGHT` | Set a custom initial window size, such as `1280x720`. |
| `--allow-insecure` | Explicitly allow a server offering unauthenticated `None` security. |
| `--input-debug` | Log outgoing key and pointer events for troubleshooting. |

Do not use `--input-debug` while typing passwords: it logs key events.

### Serve this desktop

Open the **Server** tab in the TopVNC window, enter a password, and click **Start server**. The tab binds to `127.0.0.1:5900` by default; click **All networks** to listen on every interface, or enter a display number to share a display other than the primary one. It shows the served size, address, and number of connected viewers, and keeps running while you use TopVNC as a viewer. Closing TopVNC stops the server. Server settings and passwords are not saved.

You can also run the server as a separate console mode. It binds to localhost by default and prompts for a VNC password:

```sh
cargo run --release -- --serve
cargo run --release -- --serve 0.0.0.0:5900
cargo run --release -- --serve 0.0.0.0:5900 --display 2
```

The second command listens on all network interfaces. Standard VNC password authentication uses only the first eight password bytes. TCP is unencrypted, so use a trusted network or a secure tunnel. To intentionally disable authentication, add `--allow-insecure`; do this only on an isolated trusted network. Clipboard text is synchronized between remote clients and the Windows system clipboard; characters outside Latin-1 are replaced with `?` when sent to viewers.

The server shares the primary display unless `--display NUMBER` selects another attached display (numbered from 1). Run it in an interactive, unlocked session: services and disconnected Remote Desktop sessions cannot capture the desktop. While Windows shows the secure desktop (UAC prompts, the lock screen), viewers keep the last image and capture resumes automatically afterward. If the display resolution changes, viewers that support the DesktopSize extension follow the new size; others, including TopVNC's own viewer for now, are disconnected and can reconnect.

#### On macOS

Hosting a Mac needs macOS 12.3 or later and two permissions, which the Server tab shows while the server is stopped:

- **Screen Recording** (*System Settings → Privacy & Security → Screen & System Audio Recording*) is required. Without it the server asks for access and refuses to start; quit and relaunch after granting it.
- **Accessibility** (*System Settings → Privacy & Security → Accessibility*) lets viewers type and use the mouse. Without it the server runs view-only and shows a warning, and remote input starts as soon as access is granted, without a restart.

When TopVNC runs from a terminal, including `cargo run`, macOS grants both permissions to the terminal app, not to TopVNC. The server shares the main display, or `--display NUMBER` in ScreenCaptureKit's display order, at its native pixel size: a Retina display is served at its full pixel resolution, which makes Raw updates large. The cursor is part of the captured image. If port 5900 is already taken, macOS Screen Sharing or Remote Management may be listening on it; choose another port such as 5901. A Mac that is locked cannot be captured or unlocked through this server: viewers keep the last image, and starting the server while the Mac is locked fails.

## Controls and display settings

| Control or setting | Behavior |
| --- | --- |
| Keyboard and mouse | Send basic input to the remote desktop. |
| **F8** or **F8 SETTINGS** | Open the in-session settings panel. |
| Window edges | Drag to resize the viewer. |
| Fit | Scale the remote image to the window while preserving its aspect ratio. |
| Native | Display native pixels, centered and cropped when necessary. |
| Smooth / Sharp | Choose the scaling appearance. |
| 30 / 60 / 120 FPS | Cap local presentation rate. This does not guarantee the server's frame rate or set the network polling rate. |
| F8 button size | Adjust the on-screen settings button from 0.5× to 2.0×. |
| Disconnect | Return to the connection form. |

The initial viewer fits within the primary display by default when the remote screen is larger. Only affected regions are rescaled after framebuffer updates. The next incremental update is requested as soon as the previous update is decoded, independently of the presentation FPS limit.

## Security and saved settings

Standard VNC password authentication (security type 2) is implemented. Unauthenticated `None` security (type 1) requires an explicit checkbox or `--allow-insecure`; TopVNC does not silently downgrade to it. Both modes leave framebuffer and input traffic unencrypted. Username authentication and encrypted transport are not implemented.

The last successfully connected address and port are restored on the next launch. Authentication and transport security choices are never restored automatically.

| Client platform | Password storage implementation | Validation status |
| --- | --- | --- |
| Windows | Protected with the current user's DPAPI key. | Client compiled and tested. |
| Linux | Stored through Secret Service using `secret-tool`, when available. | Client untested. |
| macOS | Password is not saved; only the address and port are retained. | Client untested. |

## Troubleshooting

### Connecting to a Mac

The VNC password opens the screen-sharing connection. The macOS lock screen asks for that Mac user's login password, which may be different. Apple-specific authentication is not available, and the unlock transition still needs live verification.

### Frozen image or connection failure

If framebuffer data stops arriving, TopVNC requests a full refresh after five seconds, preserving partially received messages. If another five-second read wait expires during that update, it returns to the connection form with an error. This recovery is covered by simulated TCP server tests on Windows.

Address lookup/TCP connection and the RFB handshake each have a ten-second deadline. Connection and session failures are shown in the connection form.

### Input appears to be ignored

Launch with `--input-debug` to inspect outgoing key and pointer events. Avoid entering passwords or other sensitive text while logging, and remove sensitive details before sharing logs.

## Development

```sh
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets
cargo run
```

### Code layout

| Path | Purpose |
| --- | --- |
| [`src/lib.rs`](src/lib.rs) | Reusable RFB protocol handling, authentication, decoding, framebuffer state, and session logic. |
| [`src/main.rs`](src/main.rs) | Native application, session worker, input handling, and software presentation. |
| [`src/ui.rs`](src/ui.rs) | Connect and Server tabs and in-session settings UI. |
| [`src/settings.rs`](src/settings.rs) | Saved connection details and platform-specific password storage. |
| [`src/desktop_host.rs`](src/desktop_host.rs) | Platform-neutral server host logic: input ownership, Windows and macOS key mapping, capture and pointer geometry, frame hand-off, and cursor compositing. |
| [`src/windows_server.rs`](src/windows_server.rs) | Windows Desktop Duplication capture and remote keyboard/mouse injection for the Server tab and `--serve`. |
| [`src/macos_server.rs`](src/macos_server.rs) | macOS ScreenCaptureKit capture, Quartz keyboard/mouse injection, and pasteboard sync for the Server tab and `--serve`. |
| [`build.rs`](build.rs) | Weak-links ScreenCaptureKit so the app still starts on macOS releases older than 12.3. |
| [`specs/004-vnc-server/spec.md`](specs/004-vnc-server/spec.md) | Server scope, security behavior, and limitations. |
| [`specs/`](specs/) | Feature scope and acceptance criteria. |

The native frontend uses TCP and a software window. Protocol and framebuffer code are kept separate from the UI so future frontends can reuse them. A browser frontend will need a browser-compatible transport and gateway because web pages cannot open raw VNC TCP sockets.

The project uses [GitHub Spec Kit](https://github.com/github/spec-kit) with Codex integration. Workflows live in `.agents/skills/`; templates and PowerShell scripts live in `.specify/`.

For contributions, read [AGENTS.md](AGENTS.md), record scope and acceptance criteria before large features, and run the checks above. Include the client OS, server software, encoding, and reproduction steps in bug reports. Measure latency, frame pacing, CPU, and memory use before claiming performance improvements.

## Roadmap

- Validate macOS unlock transitions and full remote control against live servers.
- Validate native clients on macOS and Linux.
- Add clipboard integration, remote desktop resizing, and relative pointer input.
- Expand authentication and transport security options, plus additional encodings.
- Add GPU presentation and profile end-to-end input-to-display latency.
- Build a browser client and gateway sharing the native session behavior where web APIs allow it.
