# TopVNC

A VNC viewer written in Rust, built toward responsive remote gaming with low input latency and smooth frame presentation.

TopVNC can also serve a Windows or macOS display over RFB 3.8. The reusable server protocol is in the library; the Windows and macOS executables capture the desktop and inject remote keyboard and mouse input. Linux hosting is not implemented yet. See [`specs/004-vnc-server/spec.md`](specs/004-vnc-server/spec.md) for protocol and Windows limits, and [`specs/005-macos-server/spec.md`](specs/005-macos-server/spec.md) for the macOS host.

![TopVNC connection screen with server, authentication, encoding, window size, frame rate, and scaling controls](docs/images/topvnc-connection.png)

*TopVNC running on Windows, with connection and display settings in one window.*

**Early development:**

- **Viewer.** It was tested on Windows against a live macOS VNC server and on macOS against a live server on the local network, both before the 0.3 window rewrite. The rewritten viewer (winit and wgpu) has been run live on macOS against TopVNC's server, including pointer lock and the side buttons. On Windows it compiles but has not been run. The Linux build is untested.
- **Windows server mode.** It compiles and links, and its platform-neutral logic is tested, but live desktop capture and input injection have not been verified (an earlier attempt outside an interactive desktop was denied with `0x80070005`).
- **macOS server mode.** It has served a live display, and relative mouse motion from a viewer moved the Mac's pointer. Full remote control and unlocking are still under validation.
- Linux hosting is not implemented.
- **Measurements.** Frame rate and latency have been measured end to end between TopVNC's own server and viewer over an emulated network link ([Performance](#performance)), not yet with live capture of a game.

[Quick start](#quick-start) · [Controls](#controls-and-display-settings) · [Security](#security-and-saved-settings) · [Development](#development) · [Roadmap](#roadmap)

## Features

- **Native connection UI** with a masked password field, input validation, and connection errors shown in the window.
- **Tight encoding with JPEG** (the default), plus Raw and Zlib, with 32-bit true color and incremental framebuffer updates. Full-screen motion such as games fits in a fraction of Raw's bandwidth; see [Performance](#performance).
- **Pushed updates** (RFB ContinuousUpdates with Fence flow control): the server sends each new frame without waiting for a request, keeping at most a few frames in flight from the measured throughput. Servers without them get adaptive request pipelining.
- **Mouse look for games.**
  - When the host hides its cursor, as games do while you aim, the viewer locks and hides its pointer and sends raw relative motion, so the camera turns freely. This uses the QEMU Pointer Motion Change extension.
  - **F8** or switching to another app releases the pointer.
  - The back and forward side buttons reach the host too (ExtendedMouseButtons).
- **GPU presentation** with winit and wgpu. Each frame is presented as soon as it arrives, without waiting for vertical sync, and scaled on the GPU. Input is sent as the system delivers it.
- **Parallel encoding and decoding.** The server encodes a frame in bands on every core and sends each band as soon as it is ready. The viewer decodes JPEG bands in parallel.
- **Keyboard and mouse input** for interacting with the remote desktop. Keys are sent by their position on a US keyboard, as games expect.
- **Adjustable display** with fit-to-window or native pixels, smooth or sharp scaling, full screen, and 60 FPS, 120 FPS, or no presentation limit.
- **In-session settings** accessible through **F8**, including disconnect and a resizable on-screen settings button.
- **Saved connection details** with platform-specific password storage.
- **Experimental Windows and macOS server modes**, started from the app's Server tab or with `--serve`, with display capture, resolution-change handling, remote keyboard/mouse input, clipboard sync, and password authentication; live validation is pending.

The protocol implementation handles RFB 3.3, 3.7, and 3.8. Apple's `RFB 003.889` banner is handled through a standard RFB 3.8 fallback; Apple-specific authentication is not implemented.

## Quick start

You need a stable Rust toolchain with Cargo and native build tools for your platform. Windows and macOS are the currently tested client platforms. To connect, you also need a reachable VNC server configured for standard VNC password authentication.

```sh
git clone https://github.com/yutigi/TopVNC.git
cd TopVNC
cargo run --release
```

1. Enter the server address and port (usually `5900`). Use brackets around IPv6 addresses, such as `[::1]`.
2. Enter the server's VNC password.
3. Choose the compression (Tight JPEG for games and video, Raw for a lossless image on a fast link), the initial window size, presentation rate, and scaling mode.
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
| `--quality LEVEL` | JPEG quality level for Tight, from 0 (smallest) to 9 (best); default 6. Lower it on a slow link. |
| `--input-debug` | Log outgoing key and pointer events for troubleshooting. |

Do not use `--input-debug` while typing passwords: it logs key events.

### Serve this desktop

Open the **Server** tab in the TopVNC window, enter a password, and click **Start server**. The tab binds to `127.0.0.1:5900` by default; click **All networks** to listen on every interface, or enter a display number to share a display other than the primary one. **Served size** scales the image viewers receive, from 0.25× to 1.00× of the display's pixel size: drag the slider, or click **Full** or **Half**. Half size serves a Retina display at its size in points, a quarter of the pixels, which cuts encoding time and bandwidth for games. It shows the served size, address, and number of connected viewers, and keeps running while you use TopVNC as a viewer. Closing TopVNC stops the server. Server settings and passwords are not saved.

You can also run the server as a separate console mode. It binds to localhost by default and prompts for a VNC password:

```sh
cargo run --release -- --serve
cargo run --release -- --serve 0.0.0.0:5900
cargo run --release -- --serve 0.0.0.0:5900 --display 2
cargo run --release -- --serve 0.0.0.0:5900 --scale 0.5
cargo run --release -- --serve 0.0.0.0:5900 --mouse relative
```

The second and later commands listen on all network interfaces, and `--scale` sets the served size (0.25 to 1). `--mouse` chooses when the server asks viewers for relative mouse motion:

- `auto`, the default, asks while a Windows host's cursor stays hidden, as it is while a game turns the camera.
- `relative` always asks, for games the automatic mode misses and for Mac hosts, which cannot tell when a game hides the cursor. The viewer's pointer then stays locked for the whole session.
- `absolute` never asks.

The Server tab uses `auto`. Standard VNC password authentication uses only the first eight password bytes. TCP is unencrypted, so use a trusted network or a secure tunnel. To intentionally disable authentication, add `--allow-insecure`; do this only on an isolated trusted network. Clipboard text is synchronized between remote clients and the Windows system clipboard; characters outside Latin-1 are replaced with `?` when sent to viewers.

The server shares the primary display unless `--display NUMBER` selects another attached display (numbered from 1). Run it in an interactive, unlocked session: services and disconnected Remote Desktop sessions cannot capture the desktop. While Windows shows the secure desktop (UAC prompts, the lock screen), viewers keep the last image and capture resumes automatically afterward. If the display resolution changes, viewers that support the DesktopSize extension follow the new size; others, including TopVNC's own viewer for now, are disconnected and can reconnect.

#### On macOS

Hosting a Mac needs macOS 12.3 or later and two permissions, which the Server tab shows while the server is stopped:

- **Screen Recording** (*System Settings → Privacy & Security → Screen & System Audio Recording*) is required. Without it the server asks for access and refuses to start; quit and relaunch after granting it.
- **Accessibility** (*System Settings → Privacy & Security → Accessibility*) lets viewers type and use the mouse. Without it the server runs view-only and shows a warning, and remote input starts as soon as access is granted, without a restart.

When TopVNC runs from a terminal, including `cargo run`, macOS grants both permissions to the terminal app, not to TopVNC. The server shares the main display, or `--display NUMBER` in ScreenCaptureKit's display order, at its native pixel size by default: a Retina display is then served at its full pixel resolution, which makes updates large. For games, serve at half size and use Tight JPEG in the viewer. ScreenCaptureKit scales the image on the GPU. The cursor is part of the captured image. If port 5900 is already taken, macOS Screen Sharing or Remote Management may be listening on it; choose another port such as 5901. A Mac that is locked cannot be captured or unlocked through this server: viewers keep the last image, and starting the server while the Mac is locked fails.

## Controls and display settings

| Control or setting | Behavior |
| --- | --- |
| Keyboard and mouse | Send input to the remote desktop, including the back and forward side buttons. |
| Mouse lock | While the host hides its cursor (in a game), the pointer locks and every motion turns the camera. The title bar shows *mouse locked*. |
| **F8** or **F8 SETTINGS** | Open the in-session settings panel. This also releases a locked pointer, and the on-screen button hides while the pointer is locked. |
| Window edges | Drag to resize the viewer. |
| Fit / 1:1 pixels | Scale the remote image to the window while preserving its aspect ratio, or show native pixels, centered and cropped when necessary. |
| Full screen | Switch the viewer to full screen and back. |
| 60 FPS / 120 FPS / No limit | Cap local presentation. **No limit**, the default, presents every frame as it arrives. This does not set the server's frame rate. |
| Smooth / Sharp | Choose the scaling appearance. |
| Game mouse: Auto lock / Off | **Off** never locks the pointer: the viewer stops offering relative motion and the server sends absolute positions. |
| F8 button size | Adjust the on-screen settings button from 0.5× to 2.0×. |
| Disconnect | Return to the connection form. |

The initial viewer fits within the primary display by default when the remote screen is larger. Scaling happens on the GPU at the display's full resolution. The network thread requests updates independently of the presentation limit.

## Performance

Games and video change the whole screen every frame. A Raw 1920×1080 frame is about 8.3 MB, which limits a 1 Gbit/s link to about 15 frames per second. TopVNC's server and client both support Tight encoding:

- Flat areas stay lossless and sharp: single colors are sent as fills, and areas of up to 16 colors as palettes.
- Everything else is sent as JPEG at the viewer's quality level.
- Changed tiles are merged into bands, about one per core, which are encoded in parallel and sent as each finishes. The viewer decodes them in parallel too. At 1920×1080 and quality 6 on an M3 Max:
  - encoding takes 1.9 ms instead of 4.0 ms, and the first band leaves after about 1 ms;
  - decoding takes 0.8 ms instead of 4.2 ms.

Each frame normally waits for the viewer's request, so a Wi-Fi round trip of 10–20 ms caps the frame rate well below 60 even when the link has room. When both ends support ContinuousUpdates and Fence, as TopVNC's server and viewer do, the server pushes frames instead. A fence after each update tells it what the viewer has received, and it paces frames to the throughput it measures, so frames do not queue on a slow link.

Results from `cargo run --release --example latency_bench`, between TopVNC's server and viewer through a local proxy that emulates the link. Latency is from the server receiving a frame until the viewer has all of it. Desktop capture, display, and input are not included.

1920×1080 at 60 fps, 2 ms each way:

| Link | Encoding | Frames per second | Mean latency | Per frame |
| --- | --- | --- | --- | --- |
| 1 Gbit/s | Raw, one request per frame (0.2.0) | 14.0 | 78.4 ms | 8.2 MB |
| 1 Gbit/s | Tight quality 6, pushed | 60.2 | 8.4 ms | 262 KB |
| 1 Gbit/s | Tight quality 3, pushed | 60.2 | 7.3 ms | 97 KB |
| 150 Mbit/s | Tight quality 6, pushed | 53.8 | 27.2 ms | 262 KB |
| 150 Mbit/s | Tight quality 3, pushed | 60.2 | 11.7 ms | 97 KB |

Half-size Retina (1512×982) at 60 fps, 8 ms each way (Wi-Fi-like), against 0.2 in the same session:

| Link | Encoding | 0.2 | Now |
| --- | --- | --- | --- |
| 150 Mbit/s | Tight quality 6, pushed | 58.5 fps, 29.4 ms | 60.2 fps, 24.1 ms |
| 150 Mbit/s | Tight quality 3, pushed | 60.0 fps, 24.2 ms | 60.2 fps, 18.5 ms |
| 300 Mbit/s | Tight quality 6, pushed | 60.2 fps, 23.0 ms | 60.2 fps, 19.7 ms |

The scene is synthetic, so real games compress differently. If a link cannot keep up, lower `--quality`.

For playable results:

- **Use release builds** (`cargo run --release`, or `target/release/topvnc`) on both machines. Debug builds optimize dependencies, but TopVNC's own code runs about 2–3 times slower than in release.
- **Serve a Retina Mac at half size.** A full-size frame from a 14-inch MacBook Pro is about 3.7 times the data of a half-size one. Over a 150 Mbit/s Wi-Fi-like link (4 ms each way):

  | Served size | Frames per second | Mean latency |
  | --- | --- | --- |
  | 3024×1964 (full) | 26.3 | 55.6 ms |
  | 1512×982 (half) | 60.3 | 13.6 ms |
- **Read the title bar.** During a session, the viewer's title shows:
  - frames per second, KB per frame, and Mbit/s;
  - the encoding the server sends, and `push` when the server pushes frames;
  - the mean time from a frame's arrival to its presentation.

  If it says Raw, the server does not support Tight; without `push`, every frame waits a round trip.

Tight between TopVNC's server and client is covered by tests. Tight against third-party servers and viewers, and with live desktop capture, has not yet been validated. See [`specs/006-low-latency-gaming/spec.md`](specs/006-low-latency-gaming/spec.md) and [`specs/007-competitive-gaming/spec.md`](specs/007-competitive-gaming/spec.md) for the full measurements.

### Playing fast games

Shooters such as Fortnite need mouse look, which works only with TopVNC's server on both ends (or another server with the QEMU Pointer Motion Change extension). On top of TopVNC's own latency:

- **Run the host display at its highest refresh rate.** Windows Desktop Duplication delivers frames when the display refreshes, so a 60 Hz host adds up to 16.7 ms; 120 or 144 Hz cuts that to 7–8 ms.
- **Cap the game's frame rate a little below what the host's GPU can sustain.** Capture copies frames on the same GPU, and a GPU at 100% makes each capture wait behind the game's rendering.
- **Play in borderless windowed mode,** which Desktop Duplication captures most reliably.
- **Use a wired connection for the host.** On Wi-Fi, use Tight quality 3–4, or serve a high-resolution display at a reduced size.
- **In the viewer,** use full screen and **No limit**. On macOS, turn off *Pointer acceleration* (System Settings → Mouse) so the same hand motion always turns the camera the same amount. Set sensitivity in the game.
- **If the camera does not turn** because the game hides its cursor in a way Windows does not report, start the host with `--mouse relative`.

These settings and the Windows host's mouse handling have not yet been validated with a live game.

## Security and saved settings

Standard VNC password authentication (security type 2) is implemented. Unauthenticated `None` security (type 1) requires an explicit checkbox or `--allow-insecure`; TopVNC does not silently downgrade to it. Both modes leave framebuffer and input traffic unencrypted. Username authentication and encrypted transport are not implemented.

The last successfully connected address and port are restored on the next launch. Authentication and transport security choices are never restored automatically.

| Client platform | Password storage implementation | Validation status |
| --- | --- | --- |
| Windows | Protected with the current user's DPAPI key. | Client compiled and tested. |
| Linux | Stored through Secret Service using `secret-tool`, when available. | Client untested. |
| macOS | Password is not saved; only the address and port are retained. | Client compiled and tested. |

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
| [`src/tight.rs`](src/tight.rs) | Tight encoding: the server's Fill, palette, JPEG, and zlib encoder and the client decoder. |
| [`src/main.rs`](src/main.rs) | Native application, connection and session loops, input handling, and pointer lock. |
| [`src/window.rs`](src/window.rs) | winit event loop and wgpu presentation: windows, frame textures, and presenting without waiting for vertical sync. |
| [`src/ui.rs`](src/ui.rs) | Connect and Server tabs and in-session settings UI. |
| [`src/settings.rs`](src/settings.rs) | Saved connection details and platform-specific password storage. |
| [`src/desktop_host.rs`](src/desktop_host.rs) | Platform-neutral server host logic: input ownership, Windows and macOS key mapping, capture and pointer geometry, frame hand-off, and cursor compositing. |
| [`src/windows_server.rs`](src/windows_server.rs) | Windows Desktop Duplication capture and remote keyboard/mouse injection for the Server tab and `--serve`. |
| [`src/macos_server.rs`](src/macos_server.rs) | macOS ScreenCaptureKit capture, Quartz keyboard/mouse injection, and pasteboard sync for the Server tab and `--serve`. |
| [`build.rs`](build.rs) | Weak-links ScreenCaptureKit so the app still starts on macOS releases older than 12.3. |
| [`examples/latency_bench.rs`](examples/latency_bench.rs) | End-to-end frame rate and latency benchmark over an emulated network link. |
| [`tools/screen_latency.swift`](tools/screen_latency.swift) | macOS tool that measures the time until benchmark frames are displayed in a viewer window. |
| [`specs/004-vnc-server/spec.md`](specs/004-vnc-server/spec.md) | Server scope, security behavior, and limitations. |
| [`specs/`](specs/) | Feature scope and acceptance criteria. |

The native frontend uses TCP and a software window. Protocol and framebuffer code are kept separate from the UI so future frontends can reuse them. A browser frontend will need a browser-compatible transport and gateway because web pages cannot open raw VNC TCP sockets.

The project uses [GitHub Spec Kit](https://github.com/github/spec-kit) with Codex integration. Workflows live in `.agents/skills/`; templates and PowerShell scripts live in `.specify/`.

For contributions, read [AGENTS.md](AGENTS.md), record scope and acceptance criteria before large features, and run the checks above. Include the client OS, server software, encoding, and reproduction steps in bug reports. Measure latency, frame pacing, CPU, and memory use before claiming performance improvements.

## Roadmap

- Validate the Windows host and viewer live, including relative mouse motion and side buttons in games.
- Measure input-to-display latency with live capture.
- Add hardware H.264/HEVC video (VideoToolbox, Media Foundation or NVENC) to cut bandwidth on Wi-Fi and at 120 Hz.
- Consider UDP transport with forward error correction for Wi-Fi.
- Validate macOS unlock transitions and full remote control against live servers, and the Linux viewer.
- Add clipboard integration in the viewer, remote desktop resizing, layout-independent key codes, and a client-drawn cursor.
- Expand authentication and transport security options.
- Build a browser client and gateway sharing the native session behavior where web APIs allow it.
