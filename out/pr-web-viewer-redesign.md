The browser viewer now follows the accepted reference design, with responsive Keys, Type, and Pointer controls on touch devices and a full remote viewport on desktop.

- Restore session settings, function keys, Fit to screen, fullscreen, and modal keyboard focus handling.
- Refine landscape control placement, expand the trackpad to use available space, and keep language editing reachable in short viewports.
- Bundle local fonts, add interactive desktop/mobile previews, and document the design and acceptance criteria.
- Add offline browser behavior checks and 30 reference-derived screenshot goldens, including a Windows CI job.

Validation on Windows:
- `cargo fmt --all --check`
- `cargo test --workspace --locked`
- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- Web unit tests: 76 passed.
- Offline Chrome browser behavior checks passed.
- Reference screenshot and typography/geometry checks: 30 passed.

Browser checks use an offline session fixture. Physical-device, Safari, and live-host verification remain outside this validation. The branch comparison reports trailing whitespace in four bundled upstream font license files.
