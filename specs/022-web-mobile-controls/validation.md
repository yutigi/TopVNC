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
