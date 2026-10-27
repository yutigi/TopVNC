# Smooth gaming frames

## Problem and scope

The user reports responsive input but discontinuous, uncomfortable moving
images with a Windows 11 Pro host and macOS 26.5.1 viewer. Bandwidth, CPU,
and memory may be spent to improve motion. Preserve the separate immediate
input path, authentication, validation, and transport backpressure.

Ultra currently publishes every decoded rectangle and the network thread
writes the same texture the renderer samples. A frame can therefore mix
different update boundaries. Unrestricted immediate presentation also permits
tearing. These are code-level findings, not a confirmed diagnosis of the
user's Mac display; its refresh rate and network characteristics are unknown.

## Acceptance criteria

- Whole and Ultra publish only after a complete RFB framebuffer update has
  decoded. Ultra keeps its uncapped input/network policy, but no longer forces
  partial-frame display. Explicit Progressive remains available for users
  who accept partial updates.
- Use a platform-neutral latest-frame mailbox with at most one ready frame,
  one borrowed frame, and two reusable snapshot buffers in total. The decoder
  keeps its own authoritative framebuffer. New frames replace unread frames;
  damaged areas from skipped frames are retained. Reuse buffers and copy only
  stale/changed regions, including when a buffer returns from the renderer.
- The network thread performs no GPU texture writes. A dedicated render worker
  uploads a chosen snapshot and submits its draw, preventing a later network
  update from being folded into an earlier draw. GPU/vsync waits never run on
  the input event thread. Geometry is captured on the UI thread to avoid
  synchronous macOS main-thread queries from rendering or shutdown. Acquire a
  drawable before selecting the newest snapshot, so a vsync wait cannot age
  an already chosen frame. Resizes and minimized or occluded windows remain
  bounded and recover correctly.
- Default to synchronized Smooth presentation: prefer Mailbox when explicitly
  advertised, otherwise Fifo. Keep maximum frame latency at one. Offer Immediate
  explicitly for users who prefer it. Input remains independent of frame limits.
- Avoid a growing render backlog, per-rectangle full-frame copies, new idle
  polling, a deep jitter buffer, or synthetic/interpolated game frames.
- Show render frame rate, frame interval tail, and replaced frames separately
  from decoded/network FPS. Do not label submit timing as input-to-screen time.
- Raw updates snapshot all requested changed pixels before the first socket
  write. Later capture changes cannot replace rows midway through an update.
  Capture never holds its framebuffer lock while waiting for a slow socket.
- Add deterministic tests for coherent frame boundaries, damage retention,
  buffer reuse, producer/consumer overlap, malformed regions, resize, and
  presentation mode fallback. Measure handoff latency, pacing, CPU time,
  memory, and traffic where applicable before claiming performance gains.
- Run formatting, workspace tests, Clippy, and release build. Record platform
  validation limits, including that Metal cannot be live-tested on Windows.

## Plan

1. Implement and test the incremental latest-frame mailbox in the Rust library.
2. Route decoded updates through it and move texture writes to a separate
   render worker to preserve input responsiveness during vsync.
3. Add Smooth/Immediate presentation selection, retain the immediate input path,
   and expose useful presentation statistics.
4. Exercise the handoff under high refresh and stalled consumers, measure its
   costs and pacing, then document behavior and limits.

## Implementation and costs

Whole and Ultra now publish after `read_update_pipelined` finishes decoding
all rectangles. Progressive remains an explicit partial-image policy. The
mailbox merges unpublished damage, incrementally patches a reusable snapshot,
and never changes a snapshot leased by the renderer. A returned buffer carries
its stale regions so it can catch up from the decoder's authoritative image.
It has one ready slot and two reusable snapshot buffers, independent of how
long presentation stalls. At 1080p those snapshots add about 15.8 MiB of CPU
pixel storage; the decoder's authoritative framebuffer is another 7.9 MiB.
Resize can temporarily overlap old and new allocations.

Only `topvnc-present` uploads image textures and submits draws. It waits on a
condition variable when idle, and acquires a drawable before selecting the
newest CPU snapshot. The window/input thread passes viewport geometry and
display settings to it without credentials. Geometry for pointer mapping is
also atomic, so input never waits for the CPU snapshot-copy mutex. Input is
still sent immediately by the existing writer. No video interpolation,
extra jitter queue, automatic Raw selection, resolution reduction, or
security change was introduced.

Raw transmission now copies only the requested changed pixels into one
snapshot under the framebuffer lock and releases it before socket writes.
The regression test changes capture after reading the first update header:
the original HEAD mixes old and new pixel rows and fails the test, while the
new code retains one image and passes. Tight already snapshots its bands
together. This fixes server-side coherence without holding capture on a slow
network connection.

## Measurements (2026-10-27)

Windows, Ryzen 9 9950X3D, stable Rust 1.95.0, release builds. These validate
specific stages and invariants, not the user's physical input-to-screen path.
No live game capture or Mac/Metal viewer was available.

### CPU frame-handoff stress

`cargo run --release --example frame_pacing_bench`, and the same command with
`-- --stall-ms 50`: 1920×1080, synthetic 240 Hz producer, 120 Hz consumer,
one-second warm-up and five seconds measured. The stall is deliberately
inserted every 120 consumed frames, while a snapshot is leased. Source
pixels encode the frame number throughout the image to detect mixed frames.

| Metric | Normal | 50 ms consumer stalls |
| --- | --- | --- |
| Delivered | 120.0 FPS | 115.2 FPS |
| Mean / p95 handoff age | 4.408 / 4.985 ms | 2.864 / 4.860 ms |
| Maximum handoff age | 5.383 ms | 5.682 ms |
| Mean / p95 interval | 8.333 / 8.836 ms | 8.687 / 8.895 ms |
| Maximum interval | 9.171 ms | 59.722 ms |
| Mixed or stale snapshots | 0 | 0 |
| Reused snapshot buffers | 2 | 2 |
| Published / consumed (including warm-up) | 1441 / 719 | 1441 / 695 |
| Superseded unread snapshots | 721 | 745 |
| Process CPU time | 1.172 s | 0.719 s |
| Process wall time | 6.144 s | 6.056 s |
| Peak working set | 28.41 MiB | 28.45 MiB |

CPU and peak working set were sampled every 100 ms from the child process
with PowerShell, include warm-up and both threads, and are approximate.
Background system activity was not controlled. This benchmark has no
network, JPEG, GPU, or display. The intentional stall remains visible in the
maximum interval and lower delivered FPS; the mailbox prevents it from
turning into a growing queue of old frames. Short runs and different producer
phases explain differing ages and CPU times; they are not a latency-improvement
claim. Data movement is about 11.1 GiB over six seconds of full-frame motion.

### Windows GPU smoke

`cargo test --release --bin topvnc render_worker_smoke -- --ignored --nocapture`
opens a 640×360 window, uploads synthetic 1920×1080 motion for two seconds at
a target 240 Hz, then stops and joins the worker with no further UI pumping.
It verifies actual surface submission and worker shutdown, not visible pixels.

The surface selected Mailbox on a 240 Hz monitor. The test submitted 475
distinct images from 481 publications, with 5 superseded and 1 still pending.
The rolling interval distribution reported 240.5 FPS and 6.287 ms p95; mean
ready-to-submit time was 1.202 ms. Earlier small-texture smoke also passed.
This is not a comparison with the old viewer and does not establish physical
display latency, absence of display-side artifacts, or performance on Metal.

### Network/codec boundary

The existing direct-loopback Tight quality 6 benchmark, 1920×1080 at 120 Hz
for five measured seconds, delivered 120.2 FPS at 253.1 Mbit/s; publish-to-decode
mean / p95 were 4.496 / 5.620 ms, and frame interval p95 was 9.615 ms.
Approximate process CPU time was 26.141 s over 6.271 s wall time across all
threads; peak working set was 87.11 MiB. That benchmark excludes the new
mailbox, GPU, physical input, capture, and display. It records the unchanged
Tight wire path's cost, not an end-to-end improvement.

## Validation

- Deterministic tests cover private partial decode, skipped damage, immutable
  leased snapshots during concurrent publication, reuse of two buffers,
  malformed regions, resize during a lease, input geometry without the copy
  lock, rolling pacing bounds, and synchronized-mode fallback.
- The Raw coherence regression was run against both the original HEAD library
  in an offline temporary baseline package (fails) and the new code (passes).
- The interactive Windows GPU test passes; the real software-rendered F8 panel
  was inspected and its new controls fit without overlapping.
- macOS/Metal is wired to the advertised-mode fallback but has not been built
  or run here. No live game session, physical latency test, Linux, or web
  validation was performed. The Mac user must rebuild the viewer to try it.
- `cargo fmt --all -- --check`, Clippy with warnings denied, and the Windows
  release build pass. Excluding the unrelated baseline
  `clients_without_desktop_size_are_disconnected_on_resize` notification
  failure, 159 workspace tests pass (82 library, 77 application); three manual
  tests are ignored by the normal run. The interactive GPU test was also
  invoked explicitly and passes. The unfiltered workspace command still
  reports that single existing notification timeout.
