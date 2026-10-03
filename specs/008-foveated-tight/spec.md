# Foveated Tight for first-person games

## Scope

In a first-person game the player looks at the crosshair, at the center of the screen. Detail at the edges matters much less, yet TopVNC 0.2 encodes every pixel at the client's JPEG quality and sends the frame top to bottom. On five 1080p screenshots of real shooters, one full update at quality 6 is about 342 KB, and 380 KB on the panning frames the benchmarks below use. At 120 fps that is 330 to 365 Mbit/s, more than a 300 Mbit/s link carries. The crosshair sits in band 9 of 17, so its pixels arrive only after about 220 KB.

This feature makes the server encode Tight updates **foveated**:

- JPEG quality falls by zone around the framebuffer center.
- The center's rectangles are encoded and sent first.
- A ladder of quality steps ("rungs") follows the link's measured throughput. On a saturated link the two most compact rungs send the periphery only with every second update.

The output is standard Tight: every JPEG rectangle carries its own quantization tables. Viewers need no change.

The work lands in steps. This document records all of them, and the acceptance criteria below cover Step 1.

1. **Server (this change).** Zones, center-first order, rungs with throughput adaptation, periphery skipping, and the host options. On 2026-10-03 the user chose to bring the adaptive rungs and periphery skipping forward from Step 3 into Step 1.
2. **Viewer early present.** The viewer presents the center as soon as its rectangles are decoded, instead of after the whole update. See [Next steps](#next-steps).
3. Merged into Step 1.
4. **Codec.** Upgrade zune-jpeg, then reconsider optimized Huffman tables; measure a NEON JPEG encoder on macOS hosts.

Out of scope:

- H.264/HEVC. The zones could later map to an NVENC QP delta map. We found no per-region QP control in VideoToolbox.
- Sending the periphery at half resolution.
- Eye tracking.
- A lossless refresh of static HUD corners.
- Re-tuning the zone sizes, which needs a user study.
- Fixing `latency_bench`'s link emulator (see [Background](#background-rd-2026-10-03), item 6). The fix would change numbers published in the README and specs 006 and 007.
- Claims about TigerVNC, TurboVNC, or noVNC, which are untested with mixed-quality updates.

## Background (R&D, 2026-10-03)

An R&D session prototyped the feature (`prototype.patch` in this folder applies to `c46bb72`) and measured it. The full report with charts is at https://claude.ai/artifact/1YceXMDUiakoT9x8hZ6awX. These results are settled and were not measured again:

1. **Valid Tight.** Each Tight JPEG rectangle is a complete JPEG with its own DQT tables. TopVNC's viewer decodes mixed-quality updates correctly, verified end to end through `Session`.
2. **Bytes.** Five 1080p FPS screenshots, one full-screen update each:

   | Layout | Size | Center |
   | --- | --- | --- |
   | Today: 17 bands of 64 rows, quality 6 | 342 KB | — |
   | Balanced (fovea 6, mid 4, periphery 1) | 194 KB (−43%) | Pixel-identical to today |
   | Sharp (8 / 5 / 2) | 271 KB (−21%) | +4.3 dB luma PSNR |
   | Wi-Fi (5 / 3 / 0) | 150 KB | +3.5 dB over uniform quality 3 (162 KB) |
3. **Order.** With the center first, the crosshair region is complete after 45 KB instead of 220 KB.
4. **CPU.**
   - Encode CPU per frame on one thread falls from 15.6 to 10.9 ms; at 120 fps today's encode needs 1.9 cores.
   - Wall time on 16 threads falls from 1.91 to 1.43 ms, and the crosshair rectangles are encoded after 0.62 ms.
   - Decoding falls from 1.38 to 1.10 ms.
5. **Rejected variants:**
   - A low-pass prefilter on the periphery: −6% bytes for +26% CPU.
   - A half-resolution periphery: −20% bytes, but it needs a TopVNC-only encoding and leaves the edges soft.
   - Optimized Huffman tables: −12% bytes for +46% CPU, and they trigger the zune-jpeg bug in item 7.
   - 4:2:0 in the fovea: −7% bytes, −4.2 dB chroma at the center.
   - Periphery every second update: −17% bytes, with the edges one update older. It is used only on the two most compact rungs.
6. **`latency_bench` hides center-first gains.** It forwards each read of up to 64 KB only once its last byte would have arrived. That holds back a frame's first bytes by up to 3.5 ms at 150 Mbit/s. `examples/fovea_latency.rs` paces 8 KB slices instead.
7. **zune-jpeg 0.5.15 silently mis-decodes** valid baseline JPEGs with non-interleaved scans and 4:2:0 sampling, which jpeg-encoder writes when optimized Huffman tables are on. libjpeg and zune-jpeg 0.5.16-rc2 decode them correctly (upstream PRs #421 and #452, issue #448).
8. **No SIMD JPEG on Apple Silicon.** jpeg-encoder 0.7.1's `simd` feature is AVX2-only, so macOS hosts encode with scalar code.
9. **The prototype's adaptation overshot.** Throughput was the maximum of the last eight flow-control samples. At 300 Mbit/s on Red Eclipse it oscillated near the link's limit: center p95 12.9 ms, against 7.8 ms with fixed Balanced. Step 1 replaces the estimate (see [Rungs](#rungs-and-adaptation)).

## Acceptance criteria

### Zones and rectangle layout

- **Zones.** Every 64×64 tile belongs to the zone its center falls in. The boxes are centered on the framebuffer and measured in fractions of its height:
  - the **fovea** box is ±0.30 wide and ±0.18 tall;
  - the **mid** box is ±0.60 wide and ±0.36 tall;
  - the **periphery** is everything else.

  At 1920×1080 the fovea is x 640–1280, y 320–704 (11.9% of the pixels), and the mid box is x 320–1600, y 128–960 (39.5%). The fovea sits 28 pixels above the true center because 1080 is not a multiple of 64.
- **Layout.**
  - Changed rectangles are split at the tile grid.
  - Pieces merge only within a zone: first along a tile row, then with the run directly above, up to 64 rows in the fovea and 128 rows in the mid zone and periphery, and within Tight's 2048×256 limit. A full 1080p update is 32 rectangles.
  - Every changed pixel is sent exactly once.
- **Order.** Rectangles go fovea first, then mid, then periphery. Within a zone, the one whose center is nearest the framebuffer center goes first. The encoder threads take rectangles in this order and the session writes each as soon as it and those before it are done, as spec 007 does for bands.
- **Same block grid.** Rectangle origins are multiples of 64, so the JPEG block grid matches today's bands. A zone encoded at the client's own level is pixel-identical to today's update.

### Rungs and adaptation

- **Rungs.** Each rung gives a quality level to each zone, using TigerVNC's level table, and says how often the periphery is sent. For a client at quality level 6:

  | Rung | Fovea | Mid | Periphery | Periphery sent |
  | --- | --- | --- | --- | --- |
  | 0 Sharp | 8 (q92 4:4:4) | 5 (q77 4:2:2) | 2 (q41 4:2:0) | with every update |
  | 1 Balanced | 6 (q79 4:4:4) | 4 (q62 4:2:2) | 1 (q29 4:2:0) | with every update |
  | 2 Wi-Fi | 5 (q77 4:2:2) | 3 (q42 4:2:2) | 0 (q15 4:2:0) | with every update |
  | 3 | 5 | 3 | 0 | with every second update |
  | 4 | 4 (q62 4:2:2) | 1 (q29 4:2:0) | 0 | with every second update |
- **Anchored on the client's level.** Each zone's level is the table's plus the client's level minus 6, clamped to 0–9. Rung 1's fovea is always the level the client asked for: at level 6 it is exactly the measured Balanced rung, at TigerVNC's default of 8 it is 8 / 6 / 3, and at level 3 it is 3 / 1 / 0.
- **Per-rectangle settings.** Each rectangle is encoded with the client's Tight settings and its zone's level. Fill and palette rectangles stay lossless, as before. `src/tight.rs` is unchanged.
- **Start.** Every session starts at rung 1. It keeps its rung while `auto` turns foveation off and on, since the link has not changed.
- **Acknowledgement times.** The session's reader thread stamps each fence acknowledgement when it arrives. The writer thread can handle one several milliseconds later, after encoding an update, and then handle two back to back.
- **Delivery-rate estimate.** Recent updates' bytes divided by their total delivery time, over the last eight fenced updates. It needs a round trip measured from probe fences and at least four samples.
  - An update's delivery time is the smaller of two upper bounds on its transmission time, both from arrival stamps:
    - from when its first byte was written to the arrival of its acknowledgement, minus the probe round trip. This includes waiting for the rest of the update to be encoded, the viewer's decode, and queueing behind earlier updates.
    - the gap since the previous update's acknowledgement arrived, which includes any idle time.
  - Summing, rather than taking the largest per-update rate, keeps small updates from counting as much as large ones. A link that banks idle time delivers small updates faster than it sustains.
  - Pacing is unchanged. It still uses the throughput estimate of spec 006, which stamps acknowledgements when the writer handles them and takes the maximum of eight samples, so the latency numbers of specs 006 and 007 stand. That estimate overstates the rate when acknowledgements are handled back to back, and it does not count bytes streamed while the rest of an update was being encoded.
- **Capacity.** The bytes the link carries in one host frame interval: the delivery rate times the smoothed interval between host frames that changed pixels.
  - Each sample spanning several frames weighs as much as that many one-frame samples.
  - Gaps over 0.1 s are idle time, not frame intervals.
  - The interval is taken as at least 1/480 s.
- **Adaptation**, after each continuous update while foveating, once the rung has seen eight updates:
  - **Step down** one rung when the smoothed bytes per update exceed 90% of the capacity. The delivery rate includes the viewer's decode time, so on a link that does not bank idle time this is about 80% of what the link carries.
  - **Climb** one rung when the smoothed bytes times the measured size ratio of the next sharper rung (1.44, 1.31, 1.18, 1.3 from rung 0 down) stay under 80% of the capacity, and the hold time has passed:
    - 0.5 s after a climb;
    - 1 s after a step down;
    - after a step down within 2 s of a climb, the climb overflowed. The wait before the next climb then doubles, to at most 16 s.
  - Requested updates, and clients without fences, keep their current rung.

### Periphery every second update (rungs 3 and 4)

- Applies to continuous updates. A requested update always includes the periphery.
- With interval N, periphery tiles go with every Nth update that carries fovea or mid tiles. Otherwise they are left out and stay pending: they are not marked as seen, so the latest content follows later.
- The schedule counts **updates**, not host frames. Under congestion each update spans several host frames, and counting frames would never skip anything.
- An update with only periphery changes goes out once N host frame intervals have passed since the periphery was last sent. The session wakes at that time, so a held-back tile never waits for the 1 s idle wake.

### When foveation applies

- `ServerConfig::foveation` is a `Foveation`:
  - `Off`, the library default;
  - `Auto`, while the host asks for relative pointer motion with `VncServer::set_relative_pointer(true)`;
  - `On`, always.
- It applies only to clients that receive Tight JPEG: Tight preferred, a quality level sent, and a 32-bit true-color pixel format. Lossless Tight, Zlib, and Raw clients see no change.
- The session checks before every update, so turning it on or off takes effect with the next update.
- `Auto` follows the host's wish, not whether a client supports relative motion: a viewer without the extension watching a first-person game is still looking at a centered crosshair.
  - **Windows hosts:** with `--mouse auto` (the default), foveation follows games that hide the cursor (spec 007's 150 ms rule). `--mouse relative` keeps it on, and `--mouse absolute` keeps it off.
  - **Mac hosts:** they ask for relative motion only with `--mouse relative`, so `auto` engages only then. Use `--foveate on` otherwise.

### Host options

- `topvnc --serve --foveate auto|on|off` chooses the mode. The default is `auto`. Missing, unknown, and repeated values are rejected.
- The Server tab adds a **Foveation** row with **Auto**, **On**, and **Off** (default Auto). Like the other server settings, it is locked while the server runs and is not saved.

### Measurement

- **`examples/fovea_latency.rs`** measures end to end, through the real server, an unmodified `Session`, and a local proxy that emulates a link. The proxy paces 8 KB slices with one-way delay.
  - A producer pans a real 1920×1080 game frame like a turning camera, at a fixed rate.
  - Each frame carries black-and-white markers: its number at the crosshair; the latest input sequence number the host received, next to it; and its number again in the top-left (periphery) tile.
  - The viewer side sends a pointer event every 4 ms whose position encodes a sequence number.
  - Reported per mode and link:
    - fps and KB per frame;
    - **center**: publish to the crosshair tiles decoded;
    - **full**: publish to the whole update decoded, which is when today's viewer presents;
    - **input**: pointer event sent to its sequence number decoded at the crosshair, including the uplink and the wait for the next host frame;
    - **input → full**: the same, to the end of the update that first shows it;
    - **periphery age**: how old the periphery is when an update completes.
  - It excludes the game, capture, the viewer's GPU upload and present (0.3–0.9 ms per spec 007), and display scan-out.
- **`tight_codec_timing`** adds a foveated row for the synthetic frame.
- `specs/008-foveated-tight/rnd/fetch_frames.py` downloads the five Wikimedia screenshots the R&D used and writes 1920×1080 `.rgb` files.

## Validation status

Recorded on 2026-10-03 on an M3 Max MacBook Pro (12 performance and 4 efficiency cores), macOS 26.5.1. The machine was not idle: OBS used about 43% of a core, WindowServer 22%, a Chrome helper 21%, and screensharingd 13%. Each pair of off and on rows ran back to back under the same load.

1. **Checks.**
   - `cargo test --workspace` passes: 79 library and 60 binary tests, plus 2 ignored timing tests.
   - `cargo clippy --workspace --all-targets -- -D warnings` passes, as does Windows-target clippy (`cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`).
   - **New tests** cover:
     - the zones' tile-snapped extents at 1080p;
     - the layout at five frame sizes: every pixel covered as often as the input reports it (overlapping input included), fovea first, nearest the center first, and 32 rectangles at 1080p;
     - zone levels for clients at levels 0, 3, 6, 8, and 9;
     - frame-interval smoothing: idle gaps, restarted counts, and samples spanning several frames;
     - adaptation on simulated links: climbing on a fast link and staying, stepping down to the rung a slow link carries, holding Balanced near a link's limit, backing off longer after each climb that overflows, and staying put without a rate or frame rate;
     - the periphery schedule;
     - a foveated update that is byte-identical to Tight encoding each rectangle at its zone's level. It decodes through the client, with the fovea pixel-identical to today's bands and less error than the mid zone, which has less than the periphery;
     - `Auto` through the real server: bands, then the foveated layout after `set_relative_pointer(true)`, then bands again, for a client without the relative-motion extension;
     - held-back periphery tiles staying unacknowledged, and going out with the next update that has center tiles;
     - a TopVNC `Session` decoding foveated updates, with the center within 24 levels of the source and closer than the periphery;
     - the delivery estimate with queued updates whose acknowledgements are handled late and together;
     - the frame counter counting only changes, across resizes;
     - `--foveate` parsing, and the Server tab's layout and request.
   - **Server tab.** Rendered offscreen: the row fits between the authentication checkbox and the display field. It has not been clicked live.
2. **Codec timing** (`cargo test --release --lib tight_codec_timing -- --ignored --nocapture`, the synthetic 1920×1080 smooth, grainy frame, 16 threads). "Center" is when the rectangle holding the center pixel, and every rectangle before it, is ready to write:

   | Layout | Rectangles | Size | Encode | Center | Encode, one thread | Decode, decoder threads |
   | --- | --- | --- | --- | --- | --- | --- |
   | 17 × 64-row bands, quality 6 | 17 | 236 KB | 1.9 ms | 1.1 ms | 15.2 ms | 0.9 ms |
   | Foveated Sharp (8 / 5 / 2) | 32 | 191 KB | 1.9 ms | 0.9 ms | 11.1 ms | 1.0 ms |
   | Foveated Balanced (6 / 4 / 1) | 32 | 119 KB | 1.5 ms | 0.8 ms | 10.3 ms | 0.9 ms |
   | Foveated Wi-Fi (5 / 3 / 0) | 32 | 92 KB | 1.4 ms | 0.9 ms | 9.5 ms | 0.8 ms |

   Bands of this uniform frame finish almost together, so the center gains little here. On real frames the R&D measured the crosshair complete after 45 KB instead of 220 KB (Background, item 3).
3. **End to end** (`fovea_latency`, 120 fps source, 5 s per row after 1 s of warm-up). Each value is the mean over Xonotic, Red Eclipse 2 (Edge), and Unvanquished, with p95 in parentheses. "Input → full" is what today's viewer shows, because it presents once an update is complete; "publish → center" is what Step 2 will present.

   Clients at quality 6 (TopVNC's default):

   | Link (one-way delay) | Foveation | Delivered | KB/frame | Input → full | Publish → full | Publish → center | Periphery age |
   | --- | --- | --- | --- | --- | --- | --- | --- |
   | 1 Gbit/s (2 ms) | off | 119.8 fps | 380 | 16.5 ms (21.7) | 8.7 ms (9.9) | 7.3 ms (8.3) | 8.7 ms |
   | 1 Gbit/s (2 ms) | on | 119.7 fps | 286 | 15.5 ms (20.8) | 7.7 ms (8.5) | 5.0 ms (5.4) | 7.7 ms |
   | 300 Mbit/s (4 ms) | off | 93.4 fps | 377 | 31.7 ms (40.1) | 20.4 ms (26.2) | 16.1 ms (21.7) | 20.4 ms |
   | 300 Mbit/s (4 ms) | on | 120.0 fps | 222 | 22.7 ms (27.9) | 12.7 ms (14.5) | 7.4 ms (8.9) | 12.7 ms |
   | 150 Mbit/s (8 ms) | off | 50.0 fps | 376 | 57.4 ms (70.0) | 37.3 ms (44.8) | 28.6 ms (35.6) | 37.3 ms |
   | 150 Mbit/s (8 ms) | on | 105.9 fps | 152 | 37.0 ms (44.9) | 22.4 ms (27.8) | 15.3 ms (20.2) | 24.2 ms |
   | 75 Mbit/s (8 ms) | off | 25.3 fps | 374 | 87.5 ms (109.8) | 56.8 ms (65.2) | 39.6 ms (46.0) | 56.8 ms |
   | 75 Mbit/s (8 ms) | on | 81.9 fps | 102 | 40.8 ms (49.7) | 25.0 ms (32.4) | 16.0 ms (21.3) | 32.5 ms |

   Clients at quality 3, where the ladder shifts down three levels:

   | Link (one-way delay) | Foveation | Delivered | KB/frame | Input → full | Publish → full | Publish → center |
   | --- | --- | --- | --- | --- | --- | --- |
   | 1 Gbit/s (2 ms) | off | 119.5 fps | 174 | 14.3 ms (19.7) | 6.6 ms (7.3) | 6.0 ms (6.5) |
   | 1 Gbit/s (2 ms) | on | 119.8 fps | 148 | 14.3 ms (19.4) | 6.5 ms (7.1) | 4.9 ms (5.4) |
   | 300 Mbit/s (4 ms) | off | 120.3 fps | 173 | 21.6 ms (26.8) | 11.7 ms (13.1) | 9.8 ms (11.2) |
   | 300 Mbit/s (4 ms) | on | 120.1 fps | 148 | 21.2 ms (26.3) | 11.2 ms (12.5) | 7.7 ms (8.9) |
   | 150 Mbit/s (8 ms) | off | 100.0 fps | 173 | 38.8 ms (46.1) | 24.0 ms (29.2) | 20.1 ms (25.3) |
   | 150 Mbit/s (8 ms) | on | 110.5 fps | 116 | 34.5 ms (42.4) | 20.0 ms (25.8) | 14.8 ms (20.0) |
   | 75 Mbit/s (8 ms) | off | 53.3 fps | 171 | 56.1 ms (69.3) | 36.4 ms (45.3) | 29.2 ms (37.6) |
   | 75 Mbit/s (8 ms) | on | 97.9 fps | 79 | 37.1 ms (45.0) | 22.2 ms (28.0) | 16.0 ms (20.9) |

   - **Today's rows match the R&D's** within noise. At 120 fps the R&D measured 30.2 ms at 96 fps over 300 Mbit/s, 56.4 ms at 50 fps over 150 Mbit/s, and 87.6 ms at 25 fps over 75 Mbit/s.
   - **The center** arrives as the R&D predicted for Step 2. Input → center, the "input" column of the bench, is 12.8, 17.4, 29.9, and 32.2 ms on the four links at quality 6. The R&D's foveated-plus-early-present estimates were about 12, 16.8, 29.3, and 33.9 ms.
   - **Rungs chosen.** On 1 Gbit/s all three test frames climb to Sharp. On 300 Mbit/s, Xonotic holds Sharp, Red Eclipse alternates between Sharp and Balanced, and Unvanquished between Balanced and Wi-Fi. At 150 Mbit/s the ladder moves between rungs 1 and 3, reaching 4 on Unvanquished, and at 75 Mbit/s it sits on rungs 3 and 4. At 75 Mbit/s even rung 4 (about 100 KB) exceeds the link at 120 fps, so frames are still dropped.
   - **Periphery age** exceeds full-update latency only where rungs 3 and 4 skip it: by about 2 ms at 150 Mbit/s and 8 ms at 75 Mbit/s.
   - **Handoff item 13 is improved.** On Red Eclipse at 300 Mbit/s, center p95 is 9.1 ms at 120 fps, against 12.9 ms for the prototype's adaptation and 7.8 ms for the R&D's fixed Balanced.
   - **The remaining gap is the emulator.** The bench's proxy credits up to 2 ms of idle time, so an update after a short gap transmits faster than the nominal rate. Near saturation the delivery estimate then reads above the link: about 450 Mbit/s on the 300 Mbit/s link for Red Eclipse. The rung climbs, overflows, and steps back after eight updates; each repeat waits twice as long before climbing, up to 16 s. A link that does not bank idle time should not show this; that is untested. An earlier estimate, the maximum of per-update samples timed from the start of encoding, oscillated more, as the prototype did.
4. **Not yet checked.**
   - A live host with a real game, on Windows or macOS, including the Windows host's automatic relative mode turning foveation on and off.
   - TigerVNC, TurboVNC, and noVNC with foveated updates.
   - Real networks: every link above is emulated.
   - The Server tab's Foveation row, clicked live.

## Next steps

1. **Viewer early present (Step 2).** Track the applied area during `read_update_pipelined`. Once the central box is covered, upload it and wake the window thread; upload the rest at the end of the update. Add center latency to the title bar, and verify live on macOS. A brief seam between new center and old periphery is inherent; `AutoNoVsync` already tears.
2. **Codec (Step 4).** Upgrade zune-jpeg to 0.5.16 once it is stable, add a regression test that decodes a non-interleaved 4:2:0 JPEG, and only then consider optimized Huffman tables. Measure a NEON JPEG encoder (libjpeg-turbo) on macOS hosts.
3. **Third-party viewers.** Test TigerVNC, TurboVNC, and noVNC with foveated updates before claiming support.
4. **Pacing from arrival times.** Fence acknowledgements now carry the time they arrived, but pacing still uses the time the writer thread handled them, which can be milliseconds later. Switching would sharpen pacing, but it would change the latency numbers of specs 006 and 007.
5. **The benches' link emulator** (Background item 6). Ask before changing it. It also credits up to 2 ms of idle time, which makes the delivery estimate read high near saturation (Validation status, item 3).
6. **Encode-bound hosts.** Adaptation watches the link, not the encoder. A host too slow to encode every frame drops frames at any rung. The two most compact rungs encode a little less, but nothing steps down for that reason yet.
