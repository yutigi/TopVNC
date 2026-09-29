# Native raw VNC viewer

## Scope

Create a native Rust viewer for Windows, macOS, and Linux that connects to RFB 3.3, 3.7, and 3.8 servers over TCP, receives Raw framebuffer rectangles, displays them in a window, and forwards basic keyboard and pointer input. Accept Apple's `RFB 003.889` banner by negotiating standard RFB 3.8. Keep protocol and framebuffer code independent of the window library. Support standard VNC password authentication (security type 2); a server offering only `None` security requires the user to pass `--allow-insecure` explicitly.

## Acceptance criteria

- A compliant RFB 3.3, 3.7, or 3.8 server using `None` security can be viewed when explicitly allowed.
- A server offering VNC password authentication prompts locally without echoing the password.
- Framebuffer updates are applied incrementally and bounds checked before allocation or indexing.
- The window sends key press/release and pointer position/button events.
- A remote desktop larger than the local display initially fits on screen; users can resize the window or choose an initial size. Pointer coordinates map to the displayed image after scaling and letterboxing.
- Unsupported protocol versions, security types, encodings, and oversized dimensions produce clear errors.
- Protocol parsing and framebuffer operations have focused tests.

## Outside this slice

Apple-specific authentication, encrypted transport, compressed encodings, clipboard, resize, relative mouse mode, browser support, and performance claims require later features.
