# Ultra performance mode

> Historical implementation and measurements. The immediate per-band viewer
> policy below is superseded by [Smooth gaming frames](../011-smooth-gaming/spec.md):
> Ultra now publishes complete snapshots and defaults to synchronized motion.
> Server Ultra behavior and the framebuffer conversion optimization remain.

## Goal and scope

The requested target is **below 0.5 ms from physical input to visible remote
response**. That is an aspiration, not an achieved or guaranteed latency. The
current TCP/RFB desktop-capture pipeline cannot guarantee that target. Capture,
game rendering, transport, decode, GPU scheduling, scanout, and pixel response
all contribute; a CPU timer around presentation does not measure them all.

Add an opt-in Ultra mode for the native viewer and embedded server. Preserve
standard settings, authentication, transport security, bounds validation, and
flow control. Do not force Raw encoding or a lower resolution: either can make
latency worse or change image quality unexpectedly.

## Acceptance criteria

- `--ultra` enables the viewer mode; the connection form and F8 panel expose
  the choice. Whole and Progressive retain their existing behavior when Ultra
  is off. Ultra uploads every decoded region immediately and presents without
  a local FPS cap. Turning it off restores the user's selected cap and update
  policy. No idle busy loop is introduced.
- Repeated region arrivals coalesce into one pending window wake-up. Resizes,
  failed presentations, focus loss, and input ordering continue to work.
- `--serve --ultra` and the Server tab enable the server mode before startup.
  Tight bands are flushed individually, and flow-control deadlines below 1 ms
  are not deliberately rounded up to 1 ms. Expired pacing deadlines do not
  cause idle polling. Queue and allocation bounds stay intact.
- The benchmark can bypass its artificial link, compare Standard and Ultra,
  and report full-update and first-region publish-to-decode latency plus frame
  intervals. These exclude physical input, capture, GPU display, and scanout.
- Validate formatting, workspace tests, and Clippy. Record measured latency,
  frame pacing, CPU time, and peak process memory with workload and limitations;
  do not claim an improvement unless the evidence supports it.
- Document the mode, CPU/GPU/bandwidth tradeoffs, measurement boundaries, and
  the unverified 0.5 ms end-to-end target. Platform code that only compiles is
  not described as live-tested.

## Implementation plan

1. Add a platform-neutral mode in the library and pass it through host settings.
2. Implement immediate region uploads, uncapped presentation, and wake coalescing.
3. Add server band flushing and deadline handling without idle spinning.
4. Extend benchmark timing and direct-loopback operation, then measure both modes.
5. Run required checks and record results here and usage in README.

## Measurements (2026-10-27)

Windows, Ryzen 9 9950X3D (32 logical processors), stable Rust 1.95.0, release
build. No live desktop capture or game was used. Do not interpret these CPU
and loopback measurements as physical input-to-screen latency.

### Framebuffer conversion

The pixel conversion now iterates over validated source and destination row
slices, retaining unaligned-input support, zeroing wire padding, and validating
the complete rectangle before writing. This allows compiler vectorization
without a separate index check on each pixel. It applies in both modes.

`cargo run --release --example framebuffer_bench`, 50 warm-up iterations and
300 measured iterations per implementation, same pixels and framebuffer size:

| Size | Previous indexed mean / p95 | Row slices mean / p95 |
| --- | --- | --- |
| 1280×720 | 0.340 / 0.345 ms | 0.168 / 0.171 ms |
| 1920×1080 | 0.763 / 0.790 ms | 0.381 / 0.393 ms |

This microbenchmark excludes JPEG, network, scheduling between pipeline stages,
GPU, and display. Its p95 below 0.5 ms is a stage result only.

### Whole-pipeline loopback

`cargo run --release --example latency_bench -- --direct --mode tight6-push
--size 1920x1080 --fps 120 --seconds 5`, with `--ultra` for Ultra. Two runs per
mode, order Standard/Ultra/Ultra/Standard, one-second warm-up each. No proxy,
artificial network delay, or bandwidth cap. Both modes use the row conversion.
The client callback records decode and application only; there is no window
or GPU in this benchmark, so it does not measure Ultra's upload/presentation
policy. "First marker" ends when the top-left 64×64 marker is fully applied,
which is not necessarily the first region with a center/mouse priority.

| Metric | Standard runs 1 / 2 | Ultra runs 1 / 2 |
| --- | --- | --- |
| Delivered FPS | 120 / 120 | 120 / 120 |
| Full update mean | 4.182 / 4.096 ms | 4.163 / 3.969 ms |
| Full update p95 | 5.206 / 5.218 ms | 5.163 / 5.153 ms |
| First marker mean | 3.118 / 3.055 ms | 3.129 / 2.929 ms |
| First marker p95 | 4.143 / 3.985 ms | 3.989 / 3.805 ms |
| Frame interval mean | 8.333 / 8.331 ms | 8.332 / 8.330 ms |
| Frame interval p95 | 9.477 / 9.597 ms | 9.492 / 9.576 ms |
| Traffic | 252.7 / 252.7 Mbit/s | 252.7 / 252.7 Mbit/s |
| Process CPU seconds | 24.234 / 23.484 | 22.578 / 22.938 |
| Process wall seconds | 6.246 / 6.203 | 6.123 / 6.165 |
| Peak working set | 84.94 / 85.32 MiB | 84.81 / 85.68 MiB |

CPU time includes producer, server, client, startup, and warm-up, summed across
threads; it can exceed wall time. CPU and peak working set were sampled from
the child process every 100 ms with PowerShell and are approximate. They exclude
GPU memory. Ordinary background system activity was not controlled. Two short
runs do not establish a meaningful full-pipeline improvement. The 0.5 ms
input-to-screen goal is not achieved or verified.

## Validation

- Focused tests cover immediate Ultra uploads, wake coalescing/retry, restoring
  presentation settings, submillisecond server deadlines without idle polling,
  authentication, pixel delivery and input in a real TCP session, and unaligned
  partial rectangles with malformed lengths and nonzero padding.
- The software-rendered F8 panel was inspected: Whole, Progressive, and Ultra
  fit without overlapping. A live viewer session has not been run for this mode.
- Windows code compiles. macOS configuration wiring was updated but cannot be
  compiled or live-tested from this Windows environment. Linux and web were
  not tested here.
- `cargo test --workspace` encounters the existing
  `clients_without_desktop_size_are_disconnected_on_resize` notification timeout.
  The same isolated test fails in the original HEAD library and Tight module,
  copied into an offline baseline package. It was not changed or weakened.
- Excluding that existing failure, 148 workspace tests pass (72 library and
  76 native application tests); two existing manual timing tests are ignored.
  `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and
  `git diff --check` pass.
