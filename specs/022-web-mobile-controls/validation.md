# Validation record

2026-11-07, Windows worktree `design/mobile-controls-wireframe`.

- `npm test`: 67 tests passed, including mobile event handlers, language gestures,
  shared holds, modifier ordering, preference migration, bounded Unicode chunks,
  repeat cancellation and stale native composition events.
- `npm run build`: passed with pinned wasm-bindgen 0.2.128 and the shared Cargo
  cache through `CARGO_TARGET_DIR`.
- `npm run test:wasm`: eight tests passed against the built bindings.
- `npm run test:browser`: passed in Chromium headless shell. Used the actual app
  and CSS with an offline Worker/WASM identity fixture, without an HTTP server.
  Checked Keys/Type/Pointer at 320×568, 390×844 and 844×390; dock/globe targets and
  visibility; custom/hardware chord ordering; a saved Unicode snippet; committed
  native composition deduplication; two emulated simultaneous FPS touch holds;
  blur releases; and native focus/dock visibility during a viewport reduction
  without physical rotation. Screenshots were inspected at phone/landscape sizes.
- `cargo fmt --all -- --check`, `cargo test --workspace` and
  `cargo clippy --workspace --all-targets`: passed. Used UTF-8 console mode and
  the shared target cache. The existing ignored codec timing test remains ignored.
- `git diff --check`: passed.

The checks do not establish physical-device reach, native keyboard visibility,
Safari/other browser behavior, full device IME behavior, a live encrypted remote
session with the new UI, game compatibility or latency. Chinese/Japanese fixture
conversion was deliberately excluded; native IME and editable direct characters
are the implemented paths. No production hosting deployment was performed.

## PR 12 review follow-up — 2026-11-09

- Reproduced premature key releases when hardware and touch controls held the
  same keysym. The browser session now tracks each source separately, releases
  only after both let go, clears both on lifecycle release, and prevents text
  taps or late touch releases from cancelling hardware holds. Encrypted-session
  regressions cover movement keys, Control, Shift, both press orders and reset.
- Reproduced the deployment guard accepting a newer clean `main` commit than
  the one validated. The helper now retains the initial commit and checks it
  again immediately before publishing; a focused regression rejects changes.
- Windows workspace tests and Clippy, web and Worker tests, browser/WASM build
  checks, and the offline Chromium mobile checks were rerun for these fixes.
  The original device, live-session and performance verification limits remain.
- No production deployment was performed during this review.
