# Fast games: mouse look, side buttons, and lower latency

## Scope

Spec 006 made full-screen motion fit the link. This feature targets fast first-person and third-person shooters, such as Fortnite in build mode. Those games need three things TopVNC 0.2 could not provide, and they expose latency that desktop use hides:

- **Mouse look.** These games hide the cursor and turn the camera by reading relative mouse motion. The viewer sent absolute positions, and the Windows host injected them as absolute moves. Games ignore those or treat them as jumps, and the viewer's cursor stops at the window edge (the "invisible wall"). The window library, minifb, could not lock the pointer or read raw mouse motion.
- **Side buttons.** Build-mode players often bind wall, ramp, and edit to the back and forward mouse buttons. RFB 0.2 carried only left, middle, right, and the wheel, and minifb did not report side buttons.
- **Presentation latency.** On macOS, minifb's Metal view redraws on its own 60 Hz timer, not when a frame arrives. That added about 8 ms on average plus jitter, capped presentation at 60 fps even on a 120 Hz display, and kept up to three drawables in flight. Every frame was also scaled on the CPU (about 3 ms at 1080p).

This feature:

- replaces minifb with winit and wgpu;
- adds the QEMU Pointer Motion Change and ExtendedMouseButtons extensions to the server and client;
- streams Tight bands as they are encoded and decodes JPEG bands in parallel;
- tunes host scheduling on Windows, and capture rate and thread QoS on macOS.

Out of scope: H.264/HEVC video, UDP transport, audio, gamepads, client-side cursor rendering, the QEMU Extended Key Event message, automatic relative mode on macOS hosts, and DSCP/Wi-Fi QoS marking. See [Next steps](#next-steps).

## Latency budget

Estimated for a Windows host at 60 Hz streaming 1920×1080 at Tight quality 6 to a Mac viewer over wired gigabit Ethernet. The game's own frame time is excluded. Host capture and display composition are estimates; the other stages were measured on an M3 Max (see [Validation status](#validation-status)).

| Stage | 0.2.0 | Now |
| --- | --- | --- |
| Viewer input | Polled every 2 ms | Delivered as the system reports it |
| Mouse look | Absolute positions; unusable | Relative raw motion with pointer lock |
| Desktop Duplication frame | 8–16 ms at 60 Hz | Unchanged; run the host at 120 Hz or more |
| Host encode (quality 6) | 4.0 ms on 5 bands; sent when all were done | 1.9 ms on 17 bands; the first leaves after 1.1–1.5 ms |
| Transmission (~230 KB at 1 Gbit/s) | 1.8 ms after encoding | Overlaps encoding and decoding |
| Viewer decode | 4.2 ms, one thread | 0.8 ms on decoder threads |
| Viewer copy and scaling | ~4 ms, CPU | GPU sampling |
| Presentation | Next 60 Hz timer tick (avg 8 ms) + up to 3 drawables | 0.4–0.9 ms after upload, no vsync wait, 2 drawables |

## Acceptance criteria

### Relative pointer (QEMU Pointer Motion Change, pseudo-encoding -257)

- **Wire format** (rfbproto): the server switches modes with a FramebufferUpdate rectangle of encoding -257 whose *x-position* is 1 for absolute and 0 for relative, spanning the framebuffer as QEMU sends it. In relative mode a PointerEvent's coordinates are deltas plus 0x7FFF; a button change without motion uses 0x7FFF for both.
- **Client.**
  - Advertises -257 unless the viewer turns game mouse off. Turning it off or on in a session re-sends SetEncodings, and turning it off switches the client to absolute at once.
  - Applies a mode change as soon as it reads it, under the same lock that sends input, so every later input event uses the new mode.
  - Splits a large delta into several events within ±0x3FFF.
  - In absolute mode, relative motion is dropped; in relative mode, absolute positions are dropped. Button changes are always sent.
  - Pseudo-rectangles are not counted as frames and are exempt from framebuffer bounds checks.
- **Server.**
  - `VncServer::set_relative_pointer` sets the host's wish. A session announces it whenever it changes, and again after every SetEncodings, only to clients that advertise -257.
  - **Delivery.** Clients receiving continuous updates get the announcement at once. Other clients get it at the start of their next requested update, which a waiting request answers at once. No update arrives that a request-mode client did not ask for. (Unsolicited pseudo-updates made the client request extra updates; Raw latency at 1 Gbit/s doubled from 78 to 139 ms before this rule.)
  - **No stale motion.** Pointer events already in flight at a mode change still use the old mode, and their coordinates show which:
    - Absolute coordinates are below 8192, the framebuffer limit. Relative ones lie in 0x4000–0xBFFE, because clients keep deltas within ±0x3FFF.
    - The reader interprets every event in the mode last announced. An event in the other mode keeps its buttons and loses its motion: a stale absolute position becomes a zero delta, and a stale relative event is applied at the last absolute position.
    - So no fence or round trip is needed, and clients without fences behave the same.
  - Relative events reach the host as `ClientEvent::RelativePointer { dx, dy }`.
  - Pointer events no longer wait for the framebuffer lock: bounds come from an atomic copy of the framebuffer's generation and size.
- **Viewer.**
  - **Locking.** While the server asks for relative motion, the window is focused, and the settings panel is closed, the viewer locks and hides the pointer. It uses `CursorGrabMode::Locked`, or `Confined` where locking is unsupported (Windows), after moving the pointer to the window's center.
  - **Motion.** It then sends raw device motion (winit `DeviceEvent::MouseMotion`). Motion within one batch of events is summed, fractional remainders carry over, and pending motion is sent before the next button, wheel, or key event, so a flick and a click arrive in order.
  - **Release.** Focus loss, F8, and the server switching back to absolute release the pointer. Focus loss also releases held keys and buttons on the server.
  - The settings button is hidden while the pointer is locked, so it never covers a game. The title bar says the mouse is locked and that F8 releases it.
- **Windows host.**
  - Relative events are injected with `SendInput(MOUSEEVENTF_MOVE)` without `MOUSEEVENTF_ABSOLUTE`, so games reading raw input receive the exact deltas.
  - **Auto mode** (the default): the host asks for relative motion after the system cursor has stayed hidden for 150 ms (`GetCursorInfo` without `CURSOR_SHOWING`, or with no cursor handle). It switches back to absolute as soon as the cursor is shown.
    - A cursor suppressed by touch input counts as shown, and so does a failed query, as on the secure desktop.
    - Desktop Duplication's `PointerPosition.Visible` is not used: it is also false while the pointer is on another display.
- **macOS host.**
  - Relative events move the pointer by the delta from its current position, clamped to the display, and set `kCGMouseEventDeltaX/Y`.
  - The host never asks for relative motion on its own, because macOS has no reliable system-wide cursor-visibility state.
- **Host option.** `topvnc --serve --mouse auto|relative|absolute` chooses the mode:
  - `relative` asks for relative motion for the whole session, for games the automatic mode misses and for Mac hosts.
  - `absolute` never asks.
  - The Server tab uses `auto`.

### Extended mouse buttons (ExtendedMouseButtons, pseudo-encoding -316)

- **Client.**
  - Advertises -316.
  - After the server's empty -316 acknowledgement, it sends back and forward as an extended PointerEvent: the high bit of *button-mask* is set, and a seventh byte carries back (bit 0) and forward (bit 1).
  - Before the acknowledgement it keeps the marker bit zero, as the protocol requires, so back and forward are not sent.
- **Server.**
  - Acknowledges -316 when a client first advertises it, delivered like pointer-mode announcements.
  - Once a client advertised -316, the high bit of *button-mask* marks an extended event. Otherwise bit 7 is the back button.
  - `ClientEvent` button masks widen to `u16`, with constants `BUTTON_*`: bits 0–6 as in RFB, bit 7 back, bit 8 forward. Held-button tracking covers back and forward.
- **Injection.**
  - **Windows:** `MOUSEEVENTF_XDOWN/XUP` with `XBUTTON1` (back) and `XBUTTON2` (forward).
  - **macOS:** other-mouse events with button numbers 3 and 4, including drags while they are held.

### Viewer window, input, and presentation (winit + wgpu)

- **Window layer.** minifb is removed. One winit event loop serves the connection window and session windows, pumped with `pump_app_events`. The network thread wakes it with an `EventLoopProxy` when an update has fully reached the GPU, so the loop never polls.
- **Input.** Keyboard, mouse, and wheel events are handled in arrival order. The `keysym` mapping uses physical keys (winit `KeyCode`) with the US layout and the Shift state, as before.
  - Keypad keys send keypad keysyms (`KP_0`–`KP_9`, `KP_Enter`, and so on), so games can bind them separately from the main row.
  - Adds Caps Lock, Num Lock, Scroll Lock, Pause, Print Screen, the Menu key, and F13–F24.
  - Synthetic key events and key repeats are ignored, as before.
  - The wheel accumulates line deltas, and pixel deltas at 40 logical pixels per notch, into whole notches. Clicks and scrolling land where the pointer is, even before it has moved over the window.
- **Rendering.**
  - wgpu draws the remote framebuffer as a texture on a quad. Fit and Native geometry are unchanged, in logical pixels.
  - Smooth uses linear sampling and Sharp uses nearest. The image is drawn at the display's physical resolution.
  - The settings overlay and the connection form are drawn in software as before and shown as textures, with nearest sampling at whole-number display scales.
  - All windows share one wgpu instance, adapter, and device.
- **Uploads.** The network thread decodes into its own framebuffer and uploads each update's changed area to the texture with one `write_texture` call. Each update therefore reaches the GPU whole, and the window thread never copies or scales pixels.
- **Presentation.**
  - The window thread presents as soon as it is woken, at most at the FPS limit.
  - It uses `PresentMode::AutoNoVsync` (Immediate where supported: `displaySyncEnabled = NO` on macOS, tearing on Windows), falling back to Mailbox and then Fifo.
  - `desired_maximum_frame_latency` is 1, giving two drawables on Metal.
  - A window that cannot take a frame (not yet on screen, or occluded) is drawn again 16 ms later. A minimized or fully covered window draws nothing until it is shown.
- **Settings.**
  - The FPS choices become 60, 120, and No limit; No limit is the default. The limit caps presentation only.
  - The settings panel adds Full screen and Game mouse (Auto lock / Off).
- **Windows client.** The viewer raises the timer resolution to 1 ms while running, so limits and waits are not rounded up to 15.6 ms.

### Host and codec latency

- **Streaming encode.**
  - Changed tiles are merged into bands of about `rows / encoder threads` rows, in whole 64-row tiles, between 64 and 256 rows. On 16 threads a 1080p frame is 17 bands.
  - Each client session has encoder threads that start with its first large update and stay, so later updates start no threads.
  - Bands are written to the socket in order as soon as each one and every band before it are ready, so transmission overlaps encoding. The update's rectangle count is known up front.
- **Parallel decode.**
  - The client decodes Tight JPEG rectangles on persistent decoder threads (up to 8) while it keeps reading. Fill and zlib rectangles are decoded in order on the network thread.
  - Rectangles are applied in their wire order, so overlapping rectangles behave as before.
  - Malformed JPEG data and size mismatches are rejected as before.
  - Buffers are reused between rectangles, up to 32 MB.
- **Adaptive pipelining.** With decoding off the network thread, the wait for an update's data is about its transmission time. So the threshold for requesting the next update early rises from 6 ms to 14 ms, just under a 60 Hz frame interval.
  - With 6 ms, quality 6 at 150 Mbit/s fell to 42 fps.
  - With 14 ms it delivers 60 fps at 21 ms (0.2: 60 fps at 24 ms). Saturated Raw still does not pipeline (78 ms at 1 Gbit/s, unchanged).
- **Windows host.**
  - While serving, the host requests 1 ms timer resolution and opts out of power throttling (EcoQoS and timer-resolution throttling). It runs at `HIGH_PRIORITY_CLASS`, so capture and encoding keep pace while the game is in the foreground.
  - It raises the capture device's GPU thread priority (`IDXGIDevice::SetGPUThreadPriority(7)`) where Windows allows it.
  - Each step is best effort: a failure is reported once and serving continues. Settings are restored when serving stops.
- **macOS host.**
  - Capture runs at the display's refresh rate (`CGDisplayModeGetRefreshRate`, 120 Hz on ProMotion displays), clamped to 60–240 fps.
  - The capture queue is created at user-interactive QoS. So are the serving, listener, and input threads, and, through `ServerConfig::thread_setup`, each session's update, reader, and encoder threads, since new threads do not inherit QoS.

### Measurement

- **`latency_bench`.**
  - Measures through the client's decoder threads, with the server's streaming encoder.
  - `--serve` gains `--relative-mouse`, which asks viewers for relative motion, and `--print-input`, which prints their input events, so pointer lock can be checked without a game.
  - Served frames carry their number in 32-pixel blocks at the bottom-left, clear of the viewer's settings button. On macOS, `--publish-log PATH` records each frame's publish time in `CLOCK_UPTIME_RAW` nanoseconds.
- **On-screen latency.** `tools/screen_latency.swift` (macOS) captures a viewer's window with ScreenCaptureKit and reads each frame's number back. It reports the time from publishing a frame to its display, including window-server composition, from `SCStreamFrameInfo.displayTime` and the publish log.
- **Codec timing.** `tight_codec_timing` reports encoding on the session's encoder threads, the time to the first band, and decoding on the decoder threads.
- **Viewer title bar.** Adds the mean time from an update's upload to its presentation, and shows when the pointer is locked.

## Next steps

These were considered and deferred:

1. **Hardware H.264/HEVC (RFB encoding 50, Open H.264).** VideoToolbox on macOS, and Media Foundation or NVENC on Windows.
   - It cuts bandwidth 5–10× compared with JPEG, which matters most on Wi-Fi and at 120 Hz: JPEG quality 6 at 1080p60 is already about 125 Mbit/s.
   - It enables zero-copy capture → encode → decode → present.
   - Windows hosts cannot be tested in this environment yet.
2. **UDP transport with forward error correction**, so Wi-Fi retransmissions do not stall every frame behind a lost packet.
3. **QEMU Extended Key Event (-258)** for layout-independent scan codes.
4. **DSCP / `SO_NET_SERVICE_TYPE` marking**, so Wi-Fi gives input and video traffic priority.
5. **A client-drawn cursor (Cursor pseudo-encoding)** for zero-latency pointer feedback in menus.
6. **A Server tab control for the mouse mode**, which is command-line only for now.

## Validation status

Recorded on 2026-10-03 on an M3 Max MacBook Pro (12 performance and 4 efficiency cores), macOS 26.5.1, built-in 120 Hz display.

1. **Checks.**
   - `cargo test --workspace` passes: 63 library and 59 binary tests, plus 2 ignored timing tests.
   - `cargo clippy --workspace --all-targets -- -D warnings` passes, as does Windows-target clippy (`cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`), including the wgpu and winit viewer.
   - **New tests** cover:
     - mode announcements, their delivery to request-mode clients, and stale events in both directions;
     - clients without the extension;
     - extended PointerEvents;
     - the client's mode gating, delta splitting, extended packets, and declining relative motion;
     - a TopVNC session switching to relative motion and back against TopVNC's server;
     - the encoder pool's ordering and error recovery, and band sizing;
     - in-order application of parallel JPEG rectangles around an overlapping fill, and rejection of corrupt or mis-sized JPEG;
     - the 150 ms cursor debounce, relative clamping, side-button tracking, the capture rate, and `--mouse` parsing;
     - keysyms from physical keys, wheel notches, Fit and Native placement, and overlay clipping.
2. **Codec timing** (`cargo test --release --lib tight_codec_timing -- --ignored --nocapture`, 1920×1080 smooth grainy frame, 16 threads):

   | Bands | Quality | Size | Encode | First band | Decode, one thread | Decode, decoder threads |
   | --- | --- | --- | --- | --- | --- | --- |
   | 5 × 256 rows (0.2) | 6 | 229 KB | 4.0 ms | 4.1 ms | 4.5 ms | 1.4 ms |
   | 17 × 64 rows | 3 | 85 KB | 1.4 ms | 0.8 ms | 3.5 ms | 0.7 ms |
   | 17 × 64 rows | 6 | 236 KB | 1.9 ms | 1.5 ms | 4.2 ms | 0.8 ms |
   | 17 × 64 rows | 9 | 1599 KB | 2.8 ms | 1.6 ms | 15.2 ms | 2.7 ms |

   Decoding on the decoder threads includes writing the framebuffer.
3. **End to end** (`latency_bench`, from publishing a frame on the server to the client having decoded all of it; capture and display not included).

   1920×1080 at 60 fps, 2 ms each way (0.2 figures from spec 006):

   | Link | Mode | 0.2 | Now |
   | --- | --- | --- | --- |
   | 1 Gbit/s | Tight quality 6, push | 60.2 fps, 13.8 ms | 60.2 fps, 8.4 ms (p95 8.9) |
   | 1 Gbit/s | Tight quality 3, push | 60.2 fps, 11.5 ms | 60.2 fps, 7.3 ms |
   | 1 Gbit/s | Tight quality 9, push | 37.5 fps, 38.0 ms | 49.5 fps, 28.6 ms |
   | 150 Mbit/s | Tight quality 6, push | 42.2 fps, 32.6 ms | 53.8 fps, 27.2 ms |
   | 150 Mbit/s | Tight quality 6, adaptive | 60.2 fps, 24.2 ms | 60.2 fps, 21.3 ms |
   | 150 Mbit/s | Tight quality 3, push | 60.2 fps, 15.1 ms | 60.2 fps, 11.7 ms |
   | 1 Gbit/s / 150 Mbit/s | Raw | 78 ms / 453 ms | 78 ms / 457 ms (link-bound) |

   1512×982 (half-size Retina) at 60 fps, 8 ms each way, both versions measured in the same session:

   | Link | Mode | 0.2 | Now |
   | --- | --- | --- | --- |
   | 150 Mbit/s | Tight quality 6, push | 58.5 fps, 29.4 ms (p95 40.1) | 60.2 fps, 24.1 ms (p95 27.2) |
   | 150 Mbit/s | Tight quality 3, push | 60.0 fps, 24.2 ms | 60.2 fps, 18.5 ms |
   | 150 Mbit/s | Tight quality 9, push | 12.2 fps, 99.3 ms | 14.2 fps, 89.9 ms |
   | 300 Mbit/s | Tight quality 6, push | 60.2 fps, 23.0 ms (p95 24.5) | 60.2 fps, 19.7 ms (p95 21.8) |
   | 300 Mbit/s | Tight quality 9, push | 23.5 fps, 65.6 ms | 26.8 fps, 54.3 ms |
   | 300 Mbit/s | Tight quality 6, adaptive | 33.8 fps, 34.5 ms | 36.5 fps, 30.2 ms |
4. **Viewer, live on macOS**, against `latency_bench --serve` at 1920×1080 and 60 fps:
   - The connection form and session window render through wgpu. The session presents every frame: 60 fps, with 0.3–0.9 ms from upload to present. The settings panel renders with the new rows.
   - **Found and fixed live:**
     - The first draws after a window opens can find it not yet on screen. Drawing is now retried; before, the form could stay blank.
     - The first window's surface came from a second wgpu instance, which panicked; one instance is now shared.
     - A WGSL reserved word, `target`, was used in the shader.
   - **Pointer lock**, with `--relative-mouse --print-input`:
     - The viewer locked the pointer, and the title said so.
     - 20 mouse moves of (+7, −3), injected into the HID event stream, reached the server as relative events summing exactly to (+140, −60): 17 single events and one batch of three.
     - The back and forward buttons arrived as bits 7 and 8, and were released.
     - F8 released the lock; no motion was sent while the panel was open, and closing it locked again.
     - Switching to another app released the lock; returning locked it again.
5. **macOS host, live:**
   - `topvnc --serve --mouse relative` served the built-in display at 3024×1964.
   - A headless TopVNC client waited for the relative-motion announcement and sent (+50, +30). The system cursor moved by exactly (+50, +30) points, and back.
   - **Found and fixed live:** setting a QoS floor on the already-active capture queue crashed the host on start (`SIGTRAP` from libdispatch). The queue is now created with a QoS attribute.
6. **Not yet checked.**
   - **Windows, live** (compiled and linted only):
     - relative `SendInput` and side buttons;
     - the `GetCursorInfo` automatic mode;
     - timer resolution, power throttling, priority class, and GPU priority;
     - DX12 presentation, tearing, and the confined pointer on the viewer.
   - **Fortnite, or any real game.** Whether a game's raw-input camera follows relative `SendInput` exactly, and whether its hidden cursor turns on automatic relative mode.
   - **On-screen latency, old viewer against new**, with `tools/screen_latency.swift`. The tool compiles, but the Mac locked before it could run.
   - **The Linux viewer.**
