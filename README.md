# TopVNC

TopVNC is a Rust VNC viewer project aimed at responsive remote gaming on Windows, macOS, Linux, and the web.

## Status

This repository is initialized, but it is not yet a working VNC client. The Rust package is a starting point for shared protocol code. Platform frontends and transport adapters have not been implemented.

## Development

```powershell
cargo fmt --all
cargo test
```

The project uses [GitHub Spec Kit](https://github.com/github/spec-kit) with the Codex integration. Its workflows live in `.agents/skills/`, and its shared templates and PowerShell scripts live in `.specify/`. Start with `$speckit-constitution`, then `$speckit-specify` for the first feature.

The planned architecture keeps RFB parsing and framebuffer updates in reusable Rust code. Desktop frontends will use native networking and rendering. A browser frontend will need a browser-compatible transport and gateway, since a web page cannot connect directly to a VNC TCP socket.

See [AGENTS.md](AGENTS.md) for project goals and engineering guidelines.
