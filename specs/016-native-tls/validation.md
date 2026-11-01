# Validation

Windows, 2026-11-01. Worktree: `.worktrees/native-encrypted-connection`; branch: `feat/native-encrypted-connection`; PR base: `feat/cloudflare-web-client` at `1d2e438`.

- `cargo fmt --all` and `cargo fmt --all -- --check`: pass.
- `cargo test --workspace --offline`: pass, 259 tests passed and 7 pre-existing manual/benchmark tests ignored. No failing tests were excluded.
- `cargo clippy --workspace --all-targets --offline -- -D warnings`: pass.
- `git diff --check`: pass.
- Eleven new automated tests cover the actual native connection function, PEM host/CA loading, real TCP/TLS/RFB sessions, recording both wire directions for Raw/Zlib/Tight full/incremental images, clipboard and input confidentiality, VNC authentication, unknown/expired/wrong-name certificates, explicit None-security consent, legacy/plaintext refusal, partial records across timeouts, tamper rejection, idle-read/write concurrency, and handshake/disconnect deadlines.

The large Raw wire test exposed loss of ciphertext when rustls accepted only part of a socket read. The final implementation retains unconsumed bytes in a bounded shared receive buffer and drains plaintext between records; the same test passes for all three encodings.

The complete suite exposed the previously documented Windows `clients_without_desktop_size_are_disconnected_on_resize` failure. Windows shutdown does not cancel an already pending receive. The server input reader now uses a 100 ms idle wait and checks its stop signal while preserving partial messages, so disconnect cleanup can join the reader. The original regression passes without weakening its assertions or excluding it. Encrypted sessions also assert prompt disconnect events.

The PR workflow runs formatting, the full workspace suite, and Clippy on Windows. It has been added but its remote run remains pending until the PR is opened.

An additional `cargo check --target aarch64-apple-darwin --workspace --all-targets --offline` attempt could not reach Rust type checking: ring's build requires a cross C compiler (`cc`) unavailable on this Windows machine. This is not a macOS build or live validation. Linux execution, macOS execution, GUI clicking, live desktop capture/input injection, and performance measurements were not performed. Tests inject into protocol event queues and use synthetic framebuffers; no TLS performance improvement is claimed.
