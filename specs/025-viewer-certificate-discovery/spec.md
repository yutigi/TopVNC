# Automatic native viewer certificate lookup

## Scope

The Connect form should use a public host certificate already placed in
`certs.local` in the per-user TopVNC configuration directory without requiring
`--tls-ca` on every launch. Resolve trust for the current target on each
connection attempt, including after editing the address. Keep host credential
discovery separate from viewer trust.

## Acceptance criteria

- An explicit `--tls-ca` takes precedence. Explicit plain TCP never discovers
  or loads certificates and still rejects conflicting explicit trust options.
- Otherwise, inspect certificate-only `.pem` files in the per-user TopVNC
  configuration directory's `certs.local` folder. Match the leaf certificate's
  DNS/IP subject alternative names to the target or `--tls-server-name`.
  Filenames do not establish identity. Do not fetch or automatically trust
  certificates from a server.
- Select one matching certificate chain, accepting identical copies. Multiple
  different matches fail with guidance to use `--tls-ca`. With no match, retain
  public roots. CA-only trust files still need `--tls-ca`.
- Count only regular `.pem` files toward the 128-candidate limit and cap each
  candidate's size at 1 MiB. Ignore private keys, malformed files, oversized
  files, and unrelated certificates. Skip a missing directory or a candidate
  that disappears. Report other I/O errors with the affected path.
- Build the trust store directly from the inspected certificate chain.
- TLS still verifies the presented chain, expiry, hostname, and handshake
  signature. Failed verification never retries plaintext.
- The Connect tab names automatic per-user trust, the terminal reports the
  selected absolute discovery path, and F8 shows the active transport and
  trust source for the connection.
- Cover target changes, DNS/IP identities, duplicates, ambiguity, explicit
  overrides, malformed/key files, and real TLS connections with focused tests.

The viewer trust directory is configured per user and must be writable only by
that user. A file placed there can replace public trust for a matching target.
Automatic lookup in the working directory is limited to GUI host credentials.
Only place certificates obtained through a trusted channel in the viewer trust
directory. Last-session settings omit trust decisions and plaintext consent.

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
