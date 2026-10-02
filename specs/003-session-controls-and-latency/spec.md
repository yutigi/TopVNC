# Session controls and latency

## Scope

Remove the unused username field. Remember the last successful server address, port, and VNC password. Add a choice between Raw and Zlib RFB encodings and a size setting for the in-session F8 button. Reduce avoidable delay between framebuffer updates.

## Acceptance criteria

- The connection form has no username control; keyboard focus moves from port to password.
- A successful connection saves address and port across launches. On Windows the password is encrypted with the current user's DPAPI key; on Linux it is stored by Secret Service when `secret-tool` is available. Save failures are reported locally. Authentication and transport security choices are not restored implicitly.
- The F8 settings panel offers a continuous 0.5x–2.0x slider for the on-screen F8 button. Dragging updates its size, and its clickable region follows the visible size; the F8 key remains usable.
- Raw is the default. Selecting Zlib advertises encoding 6 and decodes a persistent zlib stream across updates. Every rectangle is bounded and validated before allocation or framebuffer indexing.
- The network worker requests the next incremental update when it finishes the previous one. The FPS selector caps presentation, not server polling. Actual latency claims require live measurement.

Spec 006 later made Tight with JPEG the default compression and replaced requesting after each update with adaptive pipelining.
