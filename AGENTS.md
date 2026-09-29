# TopVNC project instructions

## Product

TopVNC is a modern VNC client viewer written in Rust. The target platforms are Windows, macOS, Linux, and the web. Its main product goal is responsive remote gaming, with low input latency and smooth frame presentation.

## Architecture

- Keep RFB protocol parsing, framebuffer state, and input event types in reusable Rust crates without platform UI dependencies.
- Keep platform transports separate. Native clients can use TCP; browser clients need a browser-compatible transport such as WebSocket through a gateway because browsers cannot open raw TCP sockets.
- Keep presentation separate from protocol decoding so desktop and web frontends can share the same session logic.
- Design for bounded allocations, incremental framebuffer updates, and explicit backpressure. Avoid a full-frame copy for every rectangle unless measurement justifies it.
- Treat all server-supplied lengths, dimensions, encodings, and coordinates as untrusted. Validate them before allocation or indexing.
- Never silently downgrade to an unauthenticated connection. Make authentication and transport security explicit in the UI and documentation.

## Development workflow

- Use stable Rust and Cargo workspaces. Keep platform-specific code behind clear crate or target boundaries.
- Format with `cargo fmt --all` and run `cargo test --workspace` and `cargo clippy --workspace --all-targets` for code changes when those targets exist.
- Add focused tests for protocol parsing, malformed messages, framebuffer bounds, and input encoding.
- Measure latency, frame pacing, CPU use, and memory use before claiming a performance improvement.
- Maintain Spec Kit artifacts in `.specify/` and feature specifications in `specs/` once initialized. Record scope and acceptance criteria before large features.

## Initial milestones

1. Establish a secure RFB session and display incremental framebuffer updates in a native window.
2. Add keyboard, mouse, relative pointer behavior where possible, clipboard, resize, and reconnect handling.
3. Add efficient encodings and a GPU presentation path, then profile end-to-end input-to-display latency.
4. Add a browser client and gateway with the same session behavior where web APIs allow it.

Do not describe a platform or encoding as supported until it is implemented and tested there.
