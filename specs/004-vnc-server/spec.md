# VNC server

## Scope

Provide a reusable RFB server endpoint and a usable Windows host mode that captures the primary desktop and injects remote input. Keep protocol parsing independent of platform APIs.

## Acceptance criteria

- The server binds a caller-selected TCP address and reports the actual bound address.
- RFB 3.8 clients can authenticate with standard VNC password authentication, or use None security only when the host explicitly enables it.
- The server announces a bounded framebuffer and name, serves requested in-bounds rectangles as 8-, 16-, or 32-bit Raw true-color pixels for valid client pixel formats, and rejects malformed or out-of-bounds client messages.
- Incremental requests send changed 64×64 tiles clipped to the requested region. Tile revisions are acknowledged only when the request covers the whole tile; partial tile requests can repeat until a full tile is requested. Requests without pending tile changes return throttled empty updates.
- Key and pointer messages are delivered to the host through a typed event queue; pointer coordinates outside the desktop are rejected.
- Remote ClientCutText messages are delivered as bounded Latin-1 text events; the Windows host converts them to Unicode system clipboard text and watches clipboard sequence changes to publish local text back to viewers.
- Library hosts can publish bounded Latin-1 clipboard text to connected clients with `VncServer::set_clipboard_text`.
- Input state is tracked per client; disconnecting one viewer releases only its held keys and buttons.
- Shared ClientInit sessions coexist; an exclusive ClientInit closes existing sessions and prevents new shared sessions until it disconnects.
- The host can update framebuffer pixels without changing dimensions. Up to eight clients are handled independently.
- The Windows executable has a `--serve` mode that captures and orients the primary display, composites a separately reported cursor, and injects remote keyboard/mouse input.
- Authentication is password protected by default; unauthenticated mode requires an explicit flag.

## Limitations

The server supports RFB 3.8 and Raw encoding only. It does not encrypt TCP; deployments must use a secured network or tunnel. Standard VNC authentication retains its protocol limitation of using only the first eight password bytes. Clipboard text is limited to Latin-1; Windows text outside that range is replaced with `?` for viewers. The Windows host backend uses DXGI Desktop Duplication for primary-display capture; live startup has not been validated in the current environment, where access to the duplication output was denied. Same-size desktop switches recover by recreating duplication; a framebuffer size change requires restarting the server. Secure desktop and multi-monitor capture are unavailable. macOS and Linux host backends remain future work.
