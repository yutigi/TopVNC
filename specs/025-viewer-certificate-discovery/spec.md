# Automatic native viewer certificate lookup

## Scope

The Connect form should use a public host certificate already placed in
`certs.local` without requiring `--tls-ca` on every launch. Resolve trust for
the current target on each connection attempt, including after editing the
address. Keep host credential discovery separate from viewer trust.

## Acceptance criteria

- An explicit `--tls-ca` takes precedence. Explicit plain TCP never discovers
  or loads certificates and still rejects conflicting explicit trust options.
- Otherwise, inspect certificate-only `.pem` files in `./certs.local`, matching
  the certificate's DNS/IP subject alternative names to the target (or explicit
  `--tls-server-name`). Filenames do not establish identity. Do not fetch or
  automatically trust certificates from a server.
- Select one matching certificate chain, accepting identical copies. Multiple
  different matches fail with guidance to use `--tls-ca`. With no match, retain
  public roots. CA-only trust files still need `--tls-ca`.
- Bound directory inspection and file reads; ignore private keys, malformed
  files, oversized files, and unrelated certificates. Report directory access
  errors other than a missing directory.
- TLS still verifies the presented chain, expiry, hostname, and handshake
  signature. Failed verification never retries plaintext.
- The Connect tab explains automatic local trust, the terminal reports the
  selected file, and successful connections report their actual transport
  choice instead of always claiming unencrypted RFB.
- Cover target changes, DNS/IP identities, duplicates, ambiguity, explicit
  overrides, malformed/key files, and real TLS connections with focused tests.

The local directory is a user-managed trust source: only certificates obtained
through a trusted channel should be placed there. No trust decision or plaintext
consent is saved in last-session settings.

## Validation

On Windows, 2026-11-19:

- `cargo fmt --all -- --check`: passed.
- `cargo test --workspace`: passed (existing ignored tests remain ignored).
- `cargo clippy --workspace --all-targets`: passed without warnings.
- Discovery tests cover DNS/IP SANs, IPv6, arbitrary filenames, changing the
  target, an overridden TLS identity, duplicate chains, ambiguous matches,
  explicit trust/plaintext precedence, conflicting options, missing directories,
  directory errors, malformed/key/oversized files, and the entry bound.
- The native app test connects to a real loopback TLS host with discovered
  trust and VNC password authentication; wrong names, substituted certificates,
  and missing trust fail. Existing TLS tests cover expiry, plaintext peers,
  authentication, and encrypted framebuffer/input traffic.
- Inspected the public SANs in the existing office and local host certificates;
  their remote DNS/IP identities are distinct. Both include localhost/127.0.0.1,
  which correctly requires an explicit choice when both files are present.

No live connection to the office PC or GUI interaction was performed.
