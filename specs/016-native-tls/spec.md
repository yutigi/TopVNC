# Encrypted native connections

## Scope

Use direct TLS 1.3 before RFB for the native viewer and Windows/macOS desktop host. Use rustls with certificate chain and DNS/IP name validation. Keep RFB and browser crates independent of native transport. Native UI and CLI default to TLS; plaintext requires `--unencrypted` on each endpoint. TLS failures never retry plaintext. Existing library TCP APIs remain available for explicit legacy integrations and the loopback browser gateway.

Configure host PEM credentials with `--tls-cert` and `--tls-key`, and optional viewer trust with `--tls-ca` and `--tls-server-name`. These options also prefill the GUI server transport. Without host credentials, starting a TLS host fails with actionable instructions. Do not generate or trust certificates silently. VNC authentication remains separate and None still requires explicit consent. Do not persist transport overrides or credential paths in saved settings.

## Acceptance criteria

- Real loopback TCP automation exercises the native Session and VncServer over TLS, authenticates a password, decodes incremental frames, and sends keyboard/pointer input.
- A recording TCP relay observes TLS records and no RFB banner, known frame payload, clipboard text, or input messages.
- An untrusted certificate, wrong DNS/IP name, wrong VNC password, plaintext peer, stalled handshake, and malformed TLS fail closed with bounded waits.
- A reader waiting for incoming TLS records does not lock out input writes. Record processing and encrypted writes are serialized, buffers are bounded, and disconnect wakes waiting reads.
- CLI/UI accurately identify TLS and explicit plaintext; host credential errors happen before desktop capture.
- Run formatting, workspace tests, Clippy, and diff checks on Windows. Record other platform/live GUI limits without claiming untested support or performance gains.
