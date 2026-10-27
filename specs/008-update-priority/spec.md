# Screen update priority

## Scope
Provide server-side Whole screen (default), Center, Mouse, and Changes modes through the Server tab and `--priority whole|center|mouse|changes`. Whole screen preserves existing transmission order. Center and Mouse order bounded 64-pixel tiles by distance to the focus. Mouse follows each viewer independently and falls back to center before pointer input or during relative mouse look. Changes sends the most recently modified tiles first; it does not predict unseen changes. Every update still includes all requested dirty pixels, preserving background freshness and existing backpressure. Priority is chosen before starting the server. No protocol extension or third-party server control is required.

## Acceptance criteria
- Default behavior and authentication stay unchanged.
- Focus modes cover requested pixels exactly, including partial tiles and initial refreshes.
- Tight and Raw both honor ordering, with pseudo-rectangles first and bounded rectangle counts.
- Pointer positions are validated before being used and are isolated per client; resize clamps focus.
- Changes ordering uses tile revisions; no pixel copy is needed for scoring.
- Whole presentation waits for update completion; Progressive uploads the first decoded region immediately, batches subsequent regions, and flushes the remainder at completion.
- UI and CLI carry the same setting to native hosts; malformed CLI choices fail.
- Focused ordering, coverage, pointer, and configuration tests pass with workspace tests, formatting and Clippy.

## Limits
Ordering determines wire delivery, not atomic display or a guaranteed latency improvement. The viewer defaults to completed updates; an F8 Whole/Progressive choice and `--progressive` enable partial presentation with 8 ms upload batching and a final flush. Live Windows/macOS behavior and performance require separate measurement. Active-window, custom-region, and adaptive policies are deferred.

## Validation (Windows, 2026-10-26)
- `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings` pass.
- `cargo test --workspace` encounters the previously documented Windows failure in `clients_without_desktop_size_are_disconnected_on_resize` (timeout waiting for disconnect event).
- With only that test excluded, 138 tests pass and two existing timing tests remain ignored.
- New tests cover exact partial-tile pixel coverage and ordering over Raw and Tight, revision acknowledgement, pointer validation/isolation, center fallback and clamping, CLI validation, UI option propagation/geometry, and whole/progressive upload batching.
- macOS wiring is updated but has not been compiled or run here. Live visual behavior and latency/CPU/memory/bandwidth measurements remain unverified.

## Additional testing (Windows, 2026-10-26)

- Added a real TCP server/client regression test covering all four policies, initial requests, dirty incremental updates, correct pixel delivery, acknowledgement, and an empty follow-up update. It passes.
- Workspace tests with the single known resize-disconnect test excluded now pass: 66 library tests and 73 executable tests (139 total); two existing timing tests remain ignored. Formatting, Clippy with warnings denied, and diff whitespace checks pass.
- Verified the Windows Server tab visually and clicked Center, Mouse, and Changes; all selected correctly, with readable labels and no overlap. No desktop sharing was started through this form.
- The subsequent isolated viewer test could not be completed: Windows automation returned `failed to activate captured window`. Whole/Progressive behavior is covered by upload-batching tests, but live progressive display remains unverified. Test processes were stopped; the viewer test used an isolated settings directory.
- Investigated the existing resize-disconnect failure with temporary tracing (removed afterward). The update writer detects the unsupported resize and closes the socket, but cleanup waits in `reader.join()`. The input reader finishes only after the test drops its peer at timeout, delaying the disconnect event. This is separate from priority ordering and remains unresolved.

### Short streaming smoke test

The existing latency benchmark now accepts `--priority whole|center|mouse|changes`, for both measurements and `--serve` scenes. Reproduce each policy with:

```powershell
cargo run --example latency_bench -- --priority center --size 640x360 --seconds 1 --fps 30 --mbps 100 --delay-ms 2
```

All 28 combinations of four priorities and seven encoding/request modes completed without protocol errors. For Tight JPEG quality 6 with pushed updates:

| Priority | Mean full-update latency | Traffic | KB/frame |
| --- | --- | --- | --- |
| Whole | 6.4 ms | 7.9 Mbit/s | 31.7 |
| Center | 8.5 ms | 16.0 Mbit/s | 64.7 |
| Mouse (no input, center fallback) | 8.3 ms | 16.0 Mbit/s | 64.7 |
| Changes | 8.5 ms | 16.0 Mbit/s | 64.7 |

These are short debug-profile smoke measurements, not stable performance estimates. The 1 s counting window reported 31 distinct frames for the 30 fps source. The benchmark measures complete updates and excludes capture, GPU presentation, and input latency; it does not measure the time to show the first focused region. Focus policies increased bandwidth in this scene because they use smaller tiles. Raw was bandwidth-limited at approximately 14 completed frames/s in every policy. No lower latency claim follows from these results.
