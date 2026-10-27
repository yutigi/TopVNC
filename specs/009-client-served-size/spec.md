# Client control of served size

## Scope
Add a 25-100% served-size slider to F8 for updated TopVNC desktop hosts. This changes the shared captured stream, not the host display mode or the local window. All viewers share the setting; the last request wins. Third-party servers remain usable and show the control as unavailable. Preserve authentication and existing staged priority work.

## Acceptance criteria
- Negotiate a versioned TopVNC capability before sending scale requests; validate 25–100 integer percentages.
- Windows and macOS capture loops apply the requested scale and publish the new framebuffer geometry, without restarting the listener.
- Viewer handles bounded DesktopSize updates, reallocates only on size changes, refreshes the full image, updates continuous regions and pointer mapping.
- Keep F8 controls and disconnect visible in a 720-pixel-high window.
- Cover malformed resize data, capability negotiation, size changes and scale requests with focused tests; run format, workspace tests and Clippy.
- Update README with real rendered UI capture and platform validation limits.

## Protocol
Private pseudo-encoding 0x54564e53 (TVNS): a payload-free rectangle with x=y=height=0 and width=25..100 announces the applied host percentage. Only desktop hosts explicitly enabling scaling advertise it. Requests use standard Fence requests with the exact payload ASCII `TopVNC.Scale/1:` followed by one binary percentage byte. Requests require the capability and DesktopSize to have been advertised; unknown fences retain normal echo behavior. This does not change the host display mode. Older clients without DesktopSize may disconnect on a shared size change.

## Validation (Windows, 2026-10-26)
- Formatting and Clippy with warnings denied pass.
- Workspace tests excluding the previously documented `clients_without_desktop_size_are_disconnected_on_resize` timeout pass: 142 tests, two existing timing tests ignored. The full workspace run still fails that existing Windows cleanup test.
- Real TCP tests cover capability negotiation, requests before negotiation, invalid lengths/percentages, repeated shrink/grow with Raw and Tight, and requested and pushed updates; decoded pixels and final dimensions match.
- Decoder tests reject invalid resize geometry and apply following rectangles against the new bounds.
- Captured and visually inspected the actual software-rendered F8 panel through `examples/capture_session_ui.rs`; README image is a renderer capture, not a live screenshot. Windows automation launch approval timed out and isolated viewer windows were not exposed to the automation inventory, so live clicking/GPU resize and native desktop capture validation remain pending.
- macOS capture restart wiring is updated but not compiled or run here. No performance improvement is claimed.
- `examples/served_size_demo.rs` provides a five-minute loopback-only synthetic scene for manual UI validation, with no desktop capture or input injection.

## Follow-up: visible capability state
Always render Full/Half/Quarter in F8. When capability negotiation has not succeeded, render disabled buttons and an explicit updated-host requirement. Clicks remain gated by the negotiated capability. A missing entire row indicates a viewer build without this UI; disabled controls require updating/restarting the host and reconnecting.

## Slider refinement
Replace fixed presets with a 25-100% slider in 1% steps. Preview locally during dragging and send a single request on release so mouse movement cannot queue capture restarts. Cancel pending drags when focus is lost or F8 closes the panel. Keep the unavailable state explicit and refresh README captures.

## Follow-up: input loss investigation (Windows 11)
- A user reports continued video with mouse and keyboard unavailable after resizing, including after closing F8 and reconnecting. The live failure has not been reproduced or diagnosed.
- Extend the Raw/Tight, requested/pushed resize test to assert key presses/releases and pointer presses/releases reach the server after each resize, then assert input delivery after reconnecting. These checks pass; they do not exercise Windows injection.
- Check Windows `SendInput` counts, including partial acceptance and rejection with error code zero. Report the first failure and subsequent successful injection in server status without logging key contents or flooding mouse-motion notices.
- Formatting, Clippy, build, and 143 workspace tests pass with the existing disconnect-timeout test excluded; two timing tests remain ignored. The full workspace run still fails `clients_without_desktop_size_are_disconnected_on_resize` as documented above. Native injection failure and recovery need live validation using the rebuilt server's diagnostics.
