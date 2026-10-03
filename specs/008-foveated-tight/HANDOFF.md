# Handoff — 008 Foveated Tight: turn the R&D prototype into a real feature

> Paste the block below into a fresh session. It's self-contained; it points at
> real files and symbols in this repo. An R&D session on 2026-10-03 prototyped
> foveated Tight for FPS games, measured it, and published a report; **nothing is
> committed**. This follow-on writes spec 008 and lands the work in steps, starting
> with the server-side change that needs no protocol or viewer changes.

---

**Goal:** Make TopVNC's server encode Tight updates *foveated* for first-person
games: JPEG quality by zone around the screen center, where the crosshair is, and
the center rectangles written first. The default "Balanced" preset is fovea q79
4:4:4, mid q62 4:2:2, periphery q29 4:2:0. Then make the viewer present the center
as soon as it's decoded, then add adaptive presets driven by flow-control
throughput. The default end condition for this session is **Step 1** (below): spec
written, tests and clippy green, latency rows recorded. Steps 2–4 are follow-ons
if there's budget. **Not yet decided with the user — ask before building:**
(a) whether foveation turns on automatically while the host asks for relative mouse
motion (proposed); (b) the CLI/Server tab surface (`--foveate auto|on|off`
proposed); (c) the branch name. `perf/foveated-tight` was suggested but not
confirmed.

**State of the tree (READ THIS):** branch **`perf/low-latency-gaming`** at
**`c46bb72`** ("007: add mouse look, side buttons, and GPU presentation"). At
handoff time the tree was clean except the untracked `specs/008-foveated-tight/`:
this file, `prototype.patch`, and `rnd/` (two helper scripts). Run `git log
--oneline -5` and `git status` first. The prototype is **not applied**:
`git apply --check specs/008-foveated-tight/prototype.patch` passed against
`c46bb72`. Branch first, then `git apply` it as a starting point; don't rewrite it
from scratch. Commit or push only when the user asks. Don't `git reset --hard` or
force-push. The R&D session's scratchpad is gone; everything needed is in this
folder.

**What the prototype contains** (`prototype.patch`, 2,251 lines). Library tests:
the 63 existing ones plus 1 new one all pass.
- `src/tight.rs`:
  - `TightSettings` gains `jpeg: Option<(u8, SamplingFactor)>` (a per-rectangle override) and `optimize_huffman`.
  - `encode_jpeg` takes quality, subsampling and the Huffman flag.
  - `pub use jpeg_encoder::SamplingFactor`.
  - The module and its items became `pub`, and `TightDecoder::read_rect` lost its `#[cfg(test)]`. Both changes exist only so the R&D example can call into it; see Design §2.
- `src/lib.rs`:
  - `pub struct Foveation` with `zone()`: fovea and mid boxes as fractions of the frame height, per-zone `(quality, subsampling)`, `band_rows` per zone, `center_first`, `periphery_interval` and `adaptive`.
  - `ServerConfig.foveation` and `SessionShared.foveation`.
  - `ServerFramebuffer.frames`, a host frame counter.
  - `foveated_rects()`, `skip_periphery()`, `ADAPTIVE_PRESETS`, and `SessionEncoder::{observe_frames, adapt}`.
  - `EncodePool`/`EncodeBatch` take per-rectangle settings (`Arc<Vec<TightSettings>>`).
  - Test `foveated_rects_cover_changes_once_and_send_the_center_first`.
- `examples/fovea_study.rs`: the codec study. It times encode and decode per strategy, then writes decoded frames for scoring.
- `examples/fovea_latency.rs`: the end-to-end bench. It uses the real server and an unmodified `Session`, plus a link-emulating proxy that paces 8 KB slices. Markers at the crosshair carry the frame number and the input sequence. It reports center, full and input latency and the periphery's age.

**Already confirmed — don't re-litigate** (full numbers, crops and charts:
https://claude.ai/artifact/1YceXMDUiakoT9x8hZ6awX):
1. **Per-rectangle JPEG quality is valid Tight.** Each Tight JPEG rectangle is a
   complete JPEG with its own DQT tables. TopVNC's own viewer decodes mixed-quality
   updates correctly, verified end-to-end through `Session`. TigerVNC, TurboVNC and
   noVNC are **untested**, and AGENTS.md forbids claiming support until they are
   tested.
2. **Bytes.** Five 1080p FPS screenshots, one full-screen update each:
   - Today (17 bands of 64 rows, q79 4:4:4): 342 KB.
   - Balanced: 194 KB (**−43%**). Its fovea is **pixel-identical** to today, because rectangle origins are multiples of 8, so the 8×8 grid matches.
   - Sharp (8/5/2): 271 KB, with the center +4.3 dB luma PSNR.
   - Wi-Fi (5/3/0): 150 KB vs 162 KB for uniform q3, with the center +3.5 dB.
3. **Order.** The crosshair region is complete after **45 KB** with center-first
   order. Today it takes **220 KB**, because the crosshair sits in band 9 of 17.
4. **CPU and time.**
   - Encode CPU per frame (one thread): 15.6 → 10.9 ms, which is 1.9 cores at 120 fps today.
   - Wall time on 16 threads: 1.91 → 1.43 ms.
   - The crosshair rectangles are encoded after 0.62 ms.
   - Decode: 1.38 → 1.10 ms.
5. **End to end, 120 fps.** Input → crosshair decoded. The first column is today's
   viewer, which presents only after the whole update. The second is foveated plus
   early present (Step 2):

   | Link | Today | Foveated + early present |
   | --- | --- | --- |
   | 1 Gbit/s | 15.5 ms | ~12 ms |
   | 300 Mbit/s | 30.2 ms at 96 fps | 16.8 ms at 120 fps (Balanced) |
   | 150 Mbit/s | 56.4 ms at 50 fps | 29.3 ms at 107 fps (adaptive) |
   | 75 Mbit/s | 87.6 ms at 25 fps | 33.9 ms at 87 fps (adaptive) |

   Without Step 2, foveated input → whole update at 150 Mbit/s is 41.6 ms (Balanced)
   and 35.7 ms (adaptive). Below ~300 Mbit/s, most of the gain comes from not
   saturating the link: today's frame at 120 fps needs ~364 Mbit/s.
6. **Rectangle layout trade-off.**
   - `[64,128,128]` rows per zone gives 32 rectangles with 20 KB of JPEG headers (about 640 B each), and encodes in 1.43 ms.
   - `[128,256,256]` gives 19 rectangles with 12 KB of headers, but takes 1.94 ms. Big rectangles claimed last land on the M3 Max's 4 efficiency cores.
   - The prototype uses `[64,128,128]`.
7. **Zones at 1080p are tile-snapped.** The fovea is x 640–1280, y 320–704 (640×384, 11.9% of pixels). The mid box is x 320–1600, y 128–960 (39.5%); the periphery is 48.6%. The fovea sits 28 px above the true center because 1080 isn't a multiple of 64.
8. **Rejected variants (measured; don't redo):**
   - Low-pass prefilter on the periphery: −6% bytes, +26% CPU.
   - Half-resolution periphery: −20% bytes and 8.4 ms CPU, but it needs a TopVNC-only encoding and leaves the edges soft (SSIM 0.83 vs 0.92).
   - Optimized Huffman tables: −12% bytes, +46% CPU, and it triggers the zune-jpeg bug (item 9).
   - 4:2:0 in the fovea: −7% bytes, −4.2 dB chroma at the center.
   - Periphery every second update: −17% bytes, with the edges one update older. Useful only when the link is saturated, so it is used only as adaptive preset 4.
9. **zune-jpeg 0.5.15 bug.** It *silently* mis-decodes valid baseline JPEGs that use
   non-interleaved scans with 2×2 luma (4:2:0); luma MSE is about 13,000 and no
   error is returned. jpeg-encoder 0.7.1 writes exactly that layout when
   `set_optimized_huffman_tables(true)`. libjpeg (PIL) decodes those files correctly,
   and so does zune-jpeg **0.5.16-rc2** (tested). Upstream fixes:
   etemesi254/zune-image PRs #421 and #452, issue #448.
10. **No SIMD on Apple Silicon.** jpeg-encoder 0.7.1's `simd` feature is AVX2-only
    (its `encoder.rs` ~487), so macOS hosts encode JPEG with scalar code.
11. **`examples/latency_bench.rs` biases first-byte timing.** `pipe()` (~204;
    `busy_until +=` ~239) forwards each read of up to 64 KB only once its last byte
    would have arrived. That delays a frame's first bytes by up to 3.5 ms at
    150 Mbit/s and hides center-first gains. With 8 KB slices, Balanced's center
    latency at 75 Mbit/s went from 36 to 20 ms.
12. **Periphery skipping: two bugs already hit and fixed in the patch.**
    - (a) Counting **host frames** never skips anything under congestion, because each update spans 3 or more frames. It must count **updates**.
    - (b) Counting updates alone makes a free link send the held-back periphery as an immediate extra update. The periphery must ride along with an update that has fovea or mid tiles. A periphery-only update goes out after N frame intervals.
13. **The adaptive prototype is sound but optimistic.**
    - On 1 Gbit/s it climbs to Sharp and latency stays the same.
    - At 300 Mbit/s on Red Eclipse it oscillated near the limit: center p95 12.9 ms vs 7.8 ms for Balanced.
    - The cause is that `FlowControl::throughput` is the **max** of the last 8 samples, and the budget is 80% of link × frame interval. Use a lower percentile, a 70% target, or hysteresis.

**Known pre-existing limitations — note them, don't fix them here:**
- **The viewer waits for the whole update.** It uploads the union of changed rectangles only after the update finishes (`src/main.rs` ~1192–1214). Step 2 changes that. A brief new-center/old-periphery seam is inherent; `AutoNoVsync` already tears.
- **`latency_bench`'s emulator bias** (item 11). Fixing it changes the numbers published in the README and in specs 006/007, so ask the user first.
- **Fixed foveation assumes a centered crosshair.** That is wrong for menus, strategy games and offset third-person cameras, which is why `auto` is tied to relative mouse mode.

**Read first** (line numbers at `c46bb72`):
- `src/tight.rs`:
  - `JPEG_LEVELS` (~43) — TigerVNC's level table, mapping each level to quality and subsampling.
  - `TightSettings` (~58) — the place for the per-rectangle JPEG override.
  - `encode_rect` (~120) — order is Fill, then palette for ≤16 colors (lossless), then JPEG when a quality is set. It calls `encode_jpeg` (~228).
  - `TightDecoder::read_rect` (~273) is `#[cfg(test)]`; `read_rect_deferred` (~294) is what the session uses.
- `src/lib.rs`:
  - `SERVER_TILE_SIZE` (~22) is 64. `MAX_UPDATES_IN_FLIGHT` (~68) is 3. `SERVER_STREAM_FLUSH_BYTES` (~89) is 4 KB.
  - `ServerConfig` (~640) — add `foveation`.
  - `VncServer::set_relative_pointer` (~990) — the hook for `auto`.
  - `ServerFramebuffer` (~825) and `update_server_framebuffer_regions` (~1162) — one call is one host frame; add the frame counter here.
  - `SessionShared` (~1230) — carries the config into each session.
  - `FlowControl` (~1679) and `throughput()` (~1724), which returns the max of 8 samples.
  - `write_client_updates` (~1804). `SessionEncoder` is built at ~1830. The continuous-update branch's `prepare_update(...)` (~1937) is where `skip_periphery` must run **before** the `!rectangles.is_empty()` check, and where `adapt()` runs after each write.
  - `prepare_update` (~2325) — returns `rectangles` (with `tile_index`) and `acknowledged`. When holding the periphery back, filter **both**, or those tiles get marked as seen.
  - `band_rows` (~2473) and `tight_rects` (~2482) — today's full-width bands, which the foveated layout replaces when it's on.
  - `EncodeBatch` (~2541), `EncodePool` (~2550) and `encoder_threads()` (~2650).
  - `SessionEncoder` (~2655) and `write_tight_update` (~2667), where the layout and settings are chosen under one framebuffer lock.
- `src/main.rs`: `run_session_inner` (~1142). The network thread (~1192–1214) runs `read_update_pipelined`, accumulates `changed`, then calls `image.upload`, `arrival.mark()` and `waker.wake()`. Step 2 adds an early upload and wake once the center rectangles are applied.
- `src/window.rs`: `Layer::upload` (~198); `PresentMode::AutoNoVsync` and `desired_maximum_frame_latency: 1` (~328–330).
- After `git apply`, approximate locations:
  - `src/lib.rs`: `Foveation` (~668), `ADAPTIVE_PRESETS` (~692), `skip_periphery` (~2635), `foveated_rects` (~2683), `Adaptive`, `observe_frames` and `adapt` (~2884–2960), the foveation branch in `write_tight_update` (~2967), and the new test (~5272).
  - `examples/fovea_latency.rs`: `SLICE_BYTES` (~41) and `modes()` (~201).

**Design** (proposed; push back if the code says otherwise):
1. **Spec first** (`specs/008-foveated-tight/spec.md`). AGENTS.md requires scope and acceptance criteria before large features. Mirror the format of 006/007: Scope, Acceptance criteria, Validation status, with measured numbers. The Spec Kit workflows are in `.agents/skills/`.
2. **Step 1: server only, standard Tight** (`tight.rs`, `lib.rs`):
   - Keep the per-rectangle `TightSettings.jpeg` override.
   - **Restore `pub(crate)` on `mod tight`.** Either turn `fovea_study` into an `#[ignore]` test like `tight_codec_timing` (`lib.rs` tests, ~4824), or drop it. Don't ship a public `tight` API just for a benchmark.
   - Port `Foveation`, `foveated_rects`, per-rectangle `EncodePool` settings and center-first order.
   - Decide with the user whether `periphery_interval` and `adaptive` belong in Step 1 or Step 3.
   - Enable through `ServeOptions` and `parse_serve_arguments` (`src/desktop_host.rs` ~33 and ~232) with `--foveate auto|on|off`, and add a Server tab control (`src/ui.rs`). `auto` means on while relative pointer is requested. Confirm this first.
   - Tests:
     - coverage and order (already in the patch);
     - a round trip of a mixed-quality update through `read_update_with_encoding`;
     - held-back periphery tiles stay pending and are sent later;
     - foveation off produces exactly today's 17 bands.
3. **Step 2: viewer early present** (`src/main.rs`):
   - Track the applied area during `read_update_pipelined`. Once the central box is covered, upload that region and `waker.wake()`, then upload the rest at the end of the update. The central box is computed from the same fractions, or ends at the first rectangle outside it, since the server sends the center first.
   - Add center latency to the title stats.
   - Verify live on macOS; see the auto-memory note `macos-gui-testing` about the Mac locking.
4. **Step 3: adaptive presets** (`lib.rs`). Port `Adaptive` with a conservative throughput estimate (item 13).
5. **Step 4: codec.**
   - Upgrade zune-jpeg to 0.5.16 once it's stable.
   - Add a regression test that decodes a non-interleaved 4:2:0 JPEG from `encode_jpeg(..., optimize_huffman = true)`.
   - Only after that, consider optimized Huffman tables.
   - Measure a NEON JPEG encoder (libjpeg-turbo) on macOS hosts.

**Explicitly out of scope:**
- H.264/HEVC. It's on the roadmap; the zones would map to an NVENC QP delta map later, and VideoToolbox has no per-region QP control that we found.
- The half-resolution periphery encoding.
- Eye tracking.
- A TurboVNC-style lossless refresh of static HUD corners.
- Claims about third-party viewers without testing them.
- Re-tuning zone sizes; that needs a user study. The current zones are fovea ±0.30×±0.18 and mid ±0.60×±0.36 of the frame height.

**Gotchas carried forward:**
- **Order of operations.** `skip_periphery` before the empty check. Filter both `rectangles` and `acknowledged`. Count updates, not host frames (item 12).
- **Static center.** If only the periphery changes while the center is static, a held-back periphery can wait up to `SERVER_IDLE_WAKE_INTERVAL` (1 s) for the next wake. This is a prototype limitation.
- **Rectangle overhead.** Every rectangle costs about 640 B of JPEG tables, so don't over-split. Efficiency cores punish large rectangles that are claimed last.
- **Never enable optimized Huffman tables** with 4:2:0 on zune-jpeg 0.5.15.
- **Bench markers.** The tiles at (896,512) and (960,512) must stay tile-aligned. 16-pixel black and white blocks survive q15 4:2:0.
- **Bench noise.** Run benches on a quiet machine. `screensharingd` was using about 30% CPU during the R&D runs, and the bench shares the CPU with the encoder pool. Give each run its own process; avoid parallel builds.
- **Bench scope.** `fovea_latency` excludes the game, capture, the viewer's GPU upload and present (0.3–0.9 ms per spec 007) and display scan-out.

**Verify:**
- `cargo fmt --all && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`.
- Windows target: `cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`, as spec 007 did, if `xwin` is installed.
- Prototype sanity, after `git apply`: `cargo test --release --lib foveated` (1 test) plus the full library suite (64 tests).
- Frames: `python3 specs/008-foveated-tight/rnd/fetch_frames.py /tmp/topvnc-frames`. It downloads five Wikimedia screenshots and writes 1920×1080 `.rgb` files; it needs Pillow.
- Codec study: `cargo run --release --example fovea_study -- /tmp/topvnc-frames /tmp/fovea-study`, then `python3 specs/008-foveated-tight/rnd/score.py /tmp/fovea-study /tmp/topvnc-frames`. Expected means over the five real games: today ≈342 KB and Balanced ≈194 KB, with the same fovea PSNR (39.5 dB). Strategy indices are listed in `strategies()`.
- End to end: `cargo run --release --example fovea_latency -- --frame /tmp/topvnc-frames/xonotic.rgb --fps 120 --seconds 5 --links 1000:2,300:4,150:8,75:8 --json /tmp/e2e.jsonl --label xonotic@120`. Each row takes about 7 s. Compare against the table in item 5; the R&D numbers average Xonotic, Red Eclipse 2 (Edge) and Unvanquished.
- After Step 2: a live run of the viewer against `latency_bench --serve` (or the macOS host), and `tools/screen_latency.swift` for on-screen latency.

---
