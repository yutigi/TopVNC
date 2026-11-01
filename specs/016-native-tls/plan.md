# Implementation plan

1. Add a native-only TLS stream with split socket reads and synchronized rustls state/writes, bounded records, and handshake deadlines.
2. Thread optional TLS through the existing native Session and VncServer without changing reusable RFB parsing or browser transports.
3. Default the executable to TLS, configure PEM credentials/trust via CLI, expose transport in the connection/server UI, and retain explicit plaintext compatibility.
4. Add automated encrypted session and failure-path tests, document configuration/compatibility, run repository gates, and publish a PR against feat/cloudflare-web-client.
