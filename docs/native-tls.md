# Native encrypted connections

The native executable now requires **TLS 1.3 before RFB** on the viewer, Server tab, and `--serve` path. Certificate chain, validity, and DNS/IP identity verification are enabled. RFB authentication happens inside TLS. A certificate error, timeout, or plaintext peer ends the attempt without retrying plaintext.

This is direct TLS wrapping, not VeNCrypt or Apple's proprietary authentication. Both TopVNC endpoints must run this version. A third-party server needs a compatible TLS wrapper, or an explicitly secured legacy connection using `--unencrypted`.

## Configure a host and viewer

Use a certificate from your organization's CA, or create a self-signed certificate and explicitly distribute its public certificate to viewers through a trusted channel. Keep the private key on the host. The certificate's subject alternative names must include the DNS name or IP verified by the viewer.

For a local test with OpenSSL installed, run from the repository root:

```sh
mkdir certs.local
openssl req -x509 -newkey rsa:3072 -sha256 -nodes -days 365 -keyout certs.local/server-key.pem -out certs.local/server.pem -subj "/CN=topvnc-host" -addext "subjectAltName=DNS:topvnc-host,IP:127.0.0.1" -addext "basicConstraints=critical,CA:FALSE" -addext "extendedKeyUsage=serverAuth"
cargo run --release -- --serve 127.0.0.1:5900 --tls-cert certs.local/server.pem --tls-key certs.local/server-key.pem
```

`certs.local/` is ignored by Git. Restrict private-key access to your host account using your platform's file permissions. The key file is unencrypted so the host can read it at startup; do not share it. The server prompts locally for a VNC password.

In another terminal:

```sh
cargo run --release -- 127.0.0.1:5900 --tls-ca certs.local/server.pem
```

Enter the VNC password and click Connect. For another computer, copy **only `server.pem`** through a trusted channel and dial the host's reachable address. With the example certificate above, add `--tls-server-name topvnc-host` to verify its DNS identity while dialing that address. Prefer issuing a certificate with the actual host DNS/IP SANs. Wrong names, expired certificates, and untrusted issuers are rejected.

Without `--tls-ca`, the viewer uses bundled public WebPKI roots. A supplied CA file replaces those roots for this launch; it does not disable any verification. System-installed private CAs must also be supplied explicitly. No skip-verification option exists.

For GUI hosting, launch with `--tls-cert CERT.pem --tls-key KEY.pem`, then use the Server tab. Certificate paths are configured at launch, not edited in the form. Missing or invalid credentials are reported before desktop capture starts. On Windows the work-server launcher reads `TOPVNC_TLS_CERT` and `TOPVNC_TLS_KEY` from its environment; its `/check` checks the executable, not certificate validity.

## Explicit legacy TCP

`--unencrypted` selects plaintext for that launch and is labeled in the UI. It never follows a TLS failure automatically. It applies to both the viewer and GUI host, or to a `--serve` invocation. It cannot be combined with TLS credentials/trust settings for that endpoint. `--allow-insecure` separately permits RFB None authentication; it does not turn off TLS.

The browser gateway already encrypts browser-to-host traffic with its Noise channel and connects to a loopback-only raw RFB target. Start that target explicitly:

```sh
cargo run --release -- --serve 127.0.0.1:5900 --unencrypted
```

Keep the browser gateway's RFB target on loopback. The native `Session::connect` / `connect_with_encoding` library APIs retain their legacy TCP behavior for embedded integrations; native application code uses `connect_with_transport` with explicit settings. Saved sessions retain the address/password behavior but never save plaintext consent, certificate paths, or trust overrides.

## Automated checks

```sh
cargo test --test native_tls
cargo test --bin topvnc native_connection_tests
cargo test --lib transport::tests
```

These tests use fresh, locally generated test identities and real loopback TCP. They capture no desktop and inject no OS input:

- The native app's connection function uses the form's CA/name settings, loads PEM host credentials, authenticates VNC, receives pixels, and delivers input to the host's protocol event queue.
- A recording TCP relay checks both directions for TLS records and absence of known RFB, framebuffer, clipboard, and input plaintext. Full and incremental frames are checked through Raw, Zlib, and Tight.
- Unknown issuers, wrong names, expired certificates, wrong passwords, None authentication without consent, and plaintext peers fail closed.
- A stalled TLS handshake times out; partial TLS records survive read timeouts; tampered ciphertext closes the connection; input writes proceed while a reader waits for frames; shutdown terminates waiting readers.

`.github/workflows/native-tls.yml` runs formatting, workspace tests (including these checks), and Clippy on Windows pull requests. Windows execution is recorded in the feature validation file. macOS/Linux live TLS desktop capture and GPU presentation require separate validation. No latency or throughput improvement is claimed.

The transport uses rustls' [record processing API](https://docs.rs/rustls/latest/rustls/enum.Connection.html), bounded receive/write buffers, and a shared TLS state. Blocking socket reads occur outside the state lock; ciphertext writes remain ordered. The host input reader checks shutdown at bounded idle intervals, including on Windows where shutdown does not cancel an already pending receive.
