# Low-latency streaming for gaming

## Scope

Make full-screen motion, such as games and video, usable over a local network. Before this change the server sent only Raw 32-bit pixels: a 1920×1080 frame is about 8.3 MB, so a 1 Gbit/s link carried at most about 15 frames per second, a 100 Mbit/s link under 2, and every changed frame waited one round trip for the next request.

This feature adds Tight encoding with JPEG to the server and client, lets the client request the next update early when the link has capacity, removes stalls between the client's network thread and window thread, and lets hosts serve a display at a reduced size. A Retina display at half size is served at its size in points, a quarter of the pixels.

Out of scope: video codecs (H.264, HEVC), ContinuousUpdates and Fence congestion control, relative pointer input, GPU presentation, audio, and changing the served size while the server runs.

## Acceptance criteria

### Tight encoding (`src/tight.rs`)

- The server and client implement RFB encoding 7 (Tight) for 24-bit true color, where a TPIXEL is three bytes in red, green, blue order.
- **Server encoder.** For each rectangle:
  - A single color is sent as Fill.
  - Up to 16 colors are sent losslessly through the palette filter: one bit per pixel for two colors, otherwise one byte per pixel. This keeps text and flat interface elements sharp.
  - Anything else is sent as JPEG when the client sent a quality level pseudo-encoding (-32 to -23). Otherwise it is sent as basic zlib-compressed pixels.
  - JPEG quality and chroma subsampling follow TigerVNC's table for levels 0–9. The zlib level is the client's compression level (-256 to -247), default 1.
  - Every zlib-compressed rectangle resets and uses stream 0, so rectangles are independent and can be encoded in parallel.
- **Server rectangles.** Changed 64×64 tiles that share a full edge are merged. Merged and full-request rectangles are split to at most 2048×256 pixels: TigerVNC rejects wider Tight rectangles, and the band height bounds every compressed length below the 22-bit compact length limit.
- **Server encoding choice.** The server uses Tight when the client lists Tight before Raw and its pixel format has three 8-bit channels in a 32-bit pixel; otherwise it sends Raw as before. Pixels for one update are copied under a single framebuffer lock, so an update shows one captured frame. They are then encoded without holding the lock, on several threads when the update has at least 128K pixels.
- **Client decoder.** The client decodes Fill, JPEG, and basic compression with the copy, palette (1–256 colors), and gradient filters across four persistent zlib streams, with stream resets. It rejects unknown compression types and filters, palette indexes past the palette, zlib data that inflates to any length other than the expected one (including output held back after all input is consumed), and JPEG data whose size does not match its rectangle.
- **Client negotiation.** With Tight selected, the client advertises Tight, Zlib, and Raw, in that order, plus a quality level and compression level 1. It accepts Raw, Zlib, or Tight rectangles, so servers without Tight still work.

### Client

- **Default.** Tight with JPEG quality level 6 is the default compression. The connection form offers Raw, Zlib, and Tight JPEG, and `--quality 0-9` sets the JPEG quality level.
- **Adaptive pipelining.** The network worker sends the first full request and then reads with `Session::read_update_pipelined`, which requests each next incremental update itself.
  - While the link has spare capacity, the request goes out as soon as an update's header arrives, so the server can prepare the next frame without waiting a round trip.
  - The session times how long it blocks on the socket while reading each update's body. If that exceeds 6 ms, the link is the bottleneck and an early request would only queue a second frame behind the first. The next request is then sent after the update completes.
- **Buffered reads.** Client socket reads go through a 256 KB buffer.
- **Presentation.**
  - The network thread decodes rectangles before taking the framebuffer lock, and publishes an update's changed area only once the whole update has arrived.
  - The window thread copies the published area into its own framebuffer under the lock, then scales without holding it, so scaling never stalls the network thread.
  - Bilinear scaling blends red and blue in one multiply and green in another.
  - When the drawn image is not scaled (Native mode, or Fit at exactly the remote size), rows are copied directly.
  - Fit mode rounds the drawn size, so a window with the remote's own size draws it unscaled. Before, truncation drew one column or row short and resampled the whole image.

### Served size (hosts)

- **Option.** `ServeOptions::scale` sets the served size as a fraction of the display's pixel size, from 0.25 to 1, rounded to hundredths. Values outside the range are clamped, and a value that is not a number serves the full size. Each axis is rounded to whole pixels, with at least one pixel.
- **Command line.** `topvnc --serve --scale FACTOR` accepts 0.25 to 1 once. It rejects missing, out-of-range, and repeated values.
- **Server tab.** A **Served size** slider covers 0.25×–1.00× in hundredths, and **Full** and **Half** buttons set 1.00× and 0.50×.
  - The slider follows the mouse while the button is held.
  - Like the other server settings, it is locked while the server runs and is not saved.
  - The status shows the served size.
- **macOS.**
  - ScreenCaptureKit captures at the served size and scales on the GPU.
  - Scaled frames are copied whole, without relying on ScreenCaptureKit's dirty rectangles, and the RFB server's tile comparison finds the changes.
  - Pointer input already maps framebuffer pixels to display points through the served size.
  - The capture restarts when the display's own pixel size changes.
- **Windows.**
  - Desktop Duplication captures at the display's size.
  - An area-average downscaler recomputes only the served pixels whose source blocks overlap the damaged regions, and sends those regions.
  - Remote pointer positions map to the display pixel under the served pixel's center.
  - After a display mode change, the served size is recomputed from the new display size.

### Measurement (`examples/latency_bench.rs`)

The benchmark runs the embedded server and a TopVNC `Session` through a local TCP proxy that limits bandwidth and adds one-way delay.

- A producer publishes a panning, textured 1920×1080 scene at 60 fps. Every tile changes every frame.
- Each frame carries its number in black and white blocks that survive JPEG.
- The benchmark reports, per encoding and link:
  - delivered distinct frames per second;
  - mean and 95th-percentile latency, from publishing a frame on the server to the client having all of it;
  - server-to-client traffic.
- Desktop capture, display, and input injection are not included.

## Validation status

Recorded on 2026-10-02 on an M3 Max MacBook Pro, macOS 26.5.1:

1. `cargo test --workspace` passes (52 library tests, 1 ignored timing test, and 55 binary tests), as does `cargo clippy --workspace --all-targets -- -D warnings`. Windows-target clippy (`cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`) passes, and the Windows release build links.
2. Tight is tested in four ways:
   - Round trips for each subencoding.
   - Hand-built gradient, stream persistence, and malformed-input cases.
   - End to end between TopVNC's server and client: lossless without a quality level; within 16 levels per channel at quality 9; and a pipelined incremental update arriving without an explicit request.
   - Raw fallback for 16-bit clients.
3. **Codec timing.** `cargo test --release --lib tight_codec_timing -- --ignored --nocapture` measures one 1920×1080 smooth, grainy frame in 5 rectangles:

   | Quality | Size | Encode, parallel | Encode, one thread | Decode |
   | --- | --- | --- | --- | --- |
   | 3 | 77 KB | 3.9 ms | 13.7 ms | 4.4 ms |
   | 6 | 229 KB | 4.4 ms | 16.9 ms | 4.7 ms |
   | 9 | 1592 KB | 5.9 ms | 22.8 ms | 15.5 ms |
4. **End to end.** `cargo run --release --example latency_bench -- --seconds 5` (1920×1080, 60 fps source, 2 ms one-way delay):

   | Link | Mode | Delivered | Mean latency | p95 latency | KB/frame |
   | --- | --- | --- | --- | --- | --- |
   | 1 Gbit/s | Raw, request after each update (0.2.0) | 15.2 fps | 73.0 ms | 80.1 ms | 8191 |
   | 1 Gbit/s | Tight quality 9, adaptive | 54.6 fps | 42.1 ms | 50.6 ms | 1736 |
   | 1 Gbit/s | Tight quality 6, adaptive | 60.0 fps | 13.8 ms | 14.1 ms | 255 |
   | 1 Gbit/s | Tight quality 3, adaptive | 60.2 fps | 11.7 ms | 12.1 ms | 89 |
   | 100 Mbit/s | Raw, request after each update (0.2.0) | 1.8 fps | 669.5 ms | 676.7 ms | 7378 |
   | 100 Mbit/s | Tight quality 6, adaptive | 49.2 fps | 28.4 ms | 40.5 ms | 254 |
   | 100 Mbit/s | Tight quality 3, adaptive | 60.4 fps | 11.7 ms | 12.2 ms | 89 |

   - With a 6 ms one-way delay at 300 Mbit/s, adaptive pipelining delivered 51.3 fps at quality 6, against 37.0 fps when requesting after each update. Mean latency was 27.0 ms against 28.4 ms.
   - On a saturated link, adaptive pipelining keeps Raw at its previous latency (1 Gbit/s: 73.3 ms). Pipelining unconditionally doubled it (about 137 ms).
   - At 100 Mbit/s, saturated Raw can still alternate between early and late requests, so its p95 latency rises to about 1.3 s.
5. **Served size.** Tested: served sizes and argument parsing; downscaler block averages, damage mapping, and coverage with uneven ratios; served-to-display pointer mapping; and the slider's range, hundredths, and normalized requests. The Server tab was rendered offscreen and checked. Windows-target clippy passes with the Windows wiring.
6. **Not yet checked live.**
   - Tight against third-party servers (TigerVNC, TurboVNC, macOS Screen Sharing) and third-party viewers against the TopVNC server.
   - Latency with real desktop capture and input on Windows and macOS hosts.
   - CPU use on the host while encoding.
   - Reduced served sizes with ScreenCaptureKit and Desktop Duplication: image quality, pointer accuracy, and display changes while serving.
