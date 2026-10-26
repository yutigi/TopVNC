# VNC server

## Scope

Provide a reusable RFB server endpoint and a usable Windows host mode that captures a desktop display and injects remote input. Keep protocol parsing independent of platform APIs.

## Acceptance criteria

### Protocol (library)

- The server binds a caller-selected TCP address and reports the actual bound address.
- RFB 3.8 clients can authenticate with standard VNC password authentication, or use None security only when the host explicitly enables it.
- A client must finish the handshake, through ClientInit, within 60 seconds in total, so a slow client cannot hold a connection slot. An accept error that affects only one connection (a client that reset before it was accepted) does not stop the listener.
- The server announces a bounded framebuffer and name, serves requested in-bounds rectangles as 8-, 16-, or 32-bit Raw true-color pixels for valid client pixel formats, and rejects malformed or out-of-bounds client messages.
- Incremental requests send changed 64×64 tiles clipped to the requested region. Tile revisions are acknowledged only when the request covers the whole tile; partial tile requests can repeat until a full tile is requested. An incremental request with no pending tile changes is held until the framebuffer changes and is otherwise answered with an empty update after 50 ms.
- Each client's messages are read on a dedicated thread, so key and pointer events reach the host without waiting for framebuffer updates to be written. Update writes take the framebuffer lock per row and are sent in bounded chunks.
- Key and pointer messages are delivered to the host through a typed event queue; pointer coordinates outside the desktop are rejected.
- Remote ClientCutText messages are delivered as bounded Latin-1 text events. Library hosts can publish bounded Latin-1 clipboard text to connected clients with `VncServer::set_clipboard_text`.
- Shared ClientInit sessions coexist; an exclusive ClientInit closes existing sessions and prevents new shared sessions until it disconnects. Up to eight clients are handled independently.
- The host can replace the framebuffer (`update_framebuffer`) or report damaged regions (`update_framebuffer_regions`), in which case only tiles overlapping the damage are compared.
- The framebuffer can change dimensions. Clients that advertise the DesktopSize pseudo-encoding (-223) receive a DesktopSize rectangle in response to their next update request; clients that do not are disconnected. After a resize, update requests sized for the old framebuffer are clipped and pointer events outside the new framebuffer are dropped instead of ending the session.

### Windows host (`topvnc --serve`)

- Authentication is password protected by default; unauthenticated mode requires an explicit flag.
- The process is per-monitor DPI aware so capture sizes, cursor positions, and injected pointer coordinates are physical pixels.
- DXGI Desktop Duplication captures the primary display, or the attached display chosen with `--display NUMBER` (1-based, in DXGI enumeration order), and orients rotated displays upright.
- Only dirty and moved regions reported by Desktop Duplication are copied from the GPU; frames where only the pointer changed redraw just the old and new cursor areas. The cursor, which Desktop Duplication reports separately, is composited over the served image.
- When capture fails (desktop switch to UAC or the lock screen, display mode change, device loss), the server keeps serving the last image and recreates capture with backoff. A changed display size is served to DesktopSize clients without a restart.
- Remote input is injected on its own thread with `SendInput`: absolute pointer moves across the virtual desktop, left/middle/right buttons, vertical and horizontal wheel (RFB buttons 4–7), and keys with scan codes and extended-key flags, distinct left/right modifiers, keypad navigation keys, and media keys. Keysyms without a virtual key are typed as Unicode characters.
- Input state is tracked per client; disconnecting one viewer releases only its held keys and buttons, and shutdown releases everything still held.
- Clipboard text from viewers is converted to Unicode system clipboard text; clipboard sequence changes publish local text back to viewers.

### Server tab (app UI)

- The connection window has **Connect** and **Server** tabs. The Server tab starts and stops the same Windows host as `--serve` without a console: listen address and port (default `127.0.0.1:5900`, with *This PC only* and *All networks* shortcuts), password, optional 1-based display number (empty serves the primary display), and an *Allow none authentication* checkbox.
- A typed password always enables VNC authentication. Starting with an empty password is refused unless *Allow none authentication* is checked, and an unauthenticated server shows a persistent warning while it runs. Server form values, including the password, are never saved.
- The form is validated before starting (address and IPv6 brackets, port 1–65535, display number) and is locked while the server runs.
- The server runs on a background thread. The tab shows its phase (stopped, starting, serving, stopping), the served size and bound address, open viewer connections (including ones still authenticating), the latest capture or clipboard message, and the error that stopped it.
- The server keeps running while the app is connected to another desktop as a viewer. Closing the app stops the server and waits for it to release every remotely held key and button.
- The capture and input threads opt in to per-monitor DPI awareness individually, so the app's own windows keep their DPI behavior. The `--serve` CLI still sets it process-wide.
- On other platforms the tab is shown, reports that hosting is Windows-only, and refuses to start.

## Limitations

The server supports RFB 3.8 with Raw encoding, and Tight with JPEG as specified in spec 006. It does not encrypt TCP; deployments must use a secured network or tunnel. Standard VNC authentication retains its protocol limitation of using only the first eight password bytes. Clipboard text is limited to Latin-1; Windows text outside that range is replaced with `?` for viewers.

The secure desktop (UAC prompts, the lock screen) cannot be captured by a user process; the last image stays on screen until capture resumes. One display is served per server process, and the Server tab runs one server at a time. Printable ASCII keysyms are mapped to US-layout virtual keys. TopVNC's own viewer does not yet advertise DesktopSize, so it is disconnected when the served display changes size and must reconnect. The macOS host backend is specified in spec 005; a Linux backend remains future work.

## Validation status

The Windows backend compiles and links for `x86_64-pc-windows-msvc` (cross-built from macOS with `cargo xwin`) and passes Windows-target clippy. Protocol behavior and the platform-neutral host logic (`src/desktop_host.rs`: input ownership, key mapping, rotation and damage geometry, pixel copying, cursor compositing, argument parsing) are covered by tests that run on any platform. The Server tab's form validation, focus order, and status transitions are tested on any platform; it has been rendered and checked on macOS, where it reports that hosting is unavailable. Thread-scoped DPI awareness and the Server tab have not run on Windows. The Windows-only unit tests and live capture and input injection have not yet run on Windows: an earlier live attempt was denied access to the duplication output (`0x80070005`), which is expected outside an interactive desktop session.
