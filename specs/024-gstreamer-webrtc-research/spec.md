# GStreamer/WebRTC R&D scope

**Date:** 2026-11-11

**Status:** Research complete; architecture proposal only

**Baseline:** main, 1e6a814f9254f0a692be0e6d8fdbdab33b952d15

## Problem and scope

Determine whether GStreamer/WebRTC merits an experimental media path for
TopVNC's remote-gaming goal. Prioritize a Windows host and browser/mobile
viewer, with native Rust viewer and macOS host implications.

The user selected a research and architecture recommendation committed to the
repository. Compare the current RFB path, GStreamer/WebRTC, direct hardware
codec APIs, Sunshine/Moonlight, and WebTransport/WebCodecs. Use the supplied
[Matthew Waters talk](https://gstconf.ubicast.tv/videos/gstreamer-webrtc/)
as historical context, and current primary documentation for capabilities.

## Deliverables and acceptance criteria

- [x] Inspect the selected main revision and identify current capture,
  protocol, presentation, transport, security, and measurement boundaries.
- [x] Compare all five approaches, separating framework, codec, and transport.
- [x] Cite primary sources and distinguish documented capability, inference,
  existing repository evidence, and untested proposals.
- [x] Address browser/mobile constraints, GPU copies, bounded memory,
  backpressure, reference-frame recovery, input, identity, and relay topology.
- [x] Explain packaging and licensing dependencies without treating a
  framework's license as permission for every codec or bundled component.
- [x] Provide a proposed ADR, alternatives, consequences, and a phased
  experiment with measurable decision gates.
- [x] Record validation and unresolved questions. Make a local commit in the
  dedicated worktree; leave publication to the user.

## Exclusions

This task adds no prototype, GStreamer dependency, RFB encoding, media service,
deployment, benchmark result, or platform-support claim. Implementation needs
a separate feature specification and tests. Audio, HDR, gamepad emulation,
multi-viewer scaling, and secure-desktop media are future scope decisions.

## Artifacts

- [Research and experiment design](research.md)
- [Proposed ADR](../../docs/adr/0001-browser-gaming-media.md)
- [Validation and limitations](validation.md)
