# TopVNC

TopVNC is a Rust VNC viewer project aimed at responsive remote gaming on Windows, macOS, Linux, and the web.

## Status

An initial native viewer is implemented. It supports RFB 3.3, 3.7, and 3.8, plus Apple's `RFB 003.889` banner through a standard 3.8 fallback. It uses the Raw encoding, 32-bit true color, incremental framebuffer updates, and basic keyboard and mouse input. It has been compiled and tested on Windows and connected to a live macOS VNC server; unlocking and full remote control remain under live validation. The client has not been tested on macOS/Linux.

Standard VNC password authentication (security type 2) uses a masked password field on the connection screen. `None` security (type 1) requires an explicit checkbox (or `--allow-insecure`). Both modes leave framebuffer and input traffic unencrypted. Use an isolated network or a separately secured tunnel for sensitive sessions. Username authentication is not implemented; the username field is informational and is not sent to the server.

On a Mac, the VNC password only opens the screen-sharing connection. The macOS lock screen asks for that Mac user's login password, which may be different.

If framebuffer data stops arriving, TopVNC requests a full refresh after five seconds to recover a stale image, including during a login/unlock transition. It preserves partially received messages while waiting. If another five-second read wait expires during that update, it returns to the connection form with an error. Address lookup/TCP connection and the RFB handshake each have a ten-second deadline. Session failures are shown in the connection form. Recovery is covered by simulated TCP server tests on Windows; the macOS unlock transition still needs live verification.

## Development

```powershell
cargo fmt --all
cargo test --workspace
cargo clippy --workspace --all-targets
cargo run
```

The connection screen accepts server address, port, VNC password, initial window size, update rate (30, 60, or 120 FPS), and smooth or sharp scaling. It validates address, port, and custom size before connecting and shows connection errors in the window. You can still pass `HOST:PORT`, `--native-size`, `--window 1280x720`, and `--allow-insecure` to prefill settings. The window fits within the primary display by default when the remote screen is larger. Drag its edges to resize it. Press **F8** or click **F8 SETTINGS** while streaming to change image fit, update rate, and scaling, or disconnect. Native pixel mode centers and crops an image larger than the viewer window. Only affected regions are rescaled after framebuffer updates. Add `--input-debug` to print outgoing key and pointer events if the remote server appears to ignore input. Do not use that flag while typing passwords because it logs key events.

The project uses [GitHub Spec Kit](https://github.com/github/spec-kit) with the Codex integration. Its workflows live in `.agents/skills/`, and its shared templates and PowerShell scripts live in `.specify/`. Start with `$speckit-constitution`, then `$speckit-specify` for the first feature.

RFB parsing and framebuffer updates live in reusable Rust code. The native frontend uses TCP and a software window. Apple-specific authentication, encrypted transport, compressed encodings, clipboard, resize, relative mouse mode, GPU rendering, and browser support are future work. A browser frontend will need a browser-compatible transport and gateway, since a web page cannot connect directly to a VNC TCP socket.

See [AGENTS.md](AGENTS.md) for project goals and engineering guidelines.
