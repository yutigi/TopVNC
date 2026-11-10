# ADR-0001: Evaluate GStreamer/WebRTC as an optional gaming media path

**Status:** Proposed

**Date:** 2026-11-11

**Deciders:** TopVNC maintainer

**Scope:** Windows host to browser/mobile first

**Evidence baseline:** main at 1e6a814; [research and sources](../../specs/024-gstreamer-webrtc-research/research.md)

## Context

TopVNC's existing RFB path shares incremental decoding and framebuffer state
between native and browser viewers. Its historical synthetic Tight benchmark
shows substantial bandwidth at 1080p/120 Hz, but excludes physical capture and
display. There is no measured comparison with hardware video or WebRTC.

Browsers already expose a WebRTC media receiver. GStreamer provides native media
and encoder integration; it can reduce the amount of custom streaming machinery.
A choice of hardware encoder alone does not solve transport, authorization,
loss recovery, mobile playback or presentation latency.

The current browser security model includes independent QR-pinned host identity,
enrolled browser keys, Noise and Access checks. Video must preserve that identity
contract. The current HTTP/WebSocket gateway is not a general UDP media gateway.

## Decision proposed

Run a separately specified, opt-in, one-viewer experiment using GStreamer on the
Windows host and RTCPeerConnection/video playback in the browser. Start with
webrtcsink's raw-input hardware H.264 path, record the exact selected encoder and
memory caps, and use a custom signaller bound to the existing enrolled session.
Use webrtcbin if measured limitations of the higher-level sink prevent required
control.

Keep reusable RFB types and decoding free of GStreamer and platform dependencies.
Preserve the RFB path as a user-selected compatibility/desktop-quality mode.
Do not change defaults or silently switch authentication, software encoding,
codec, quality or media transport.

This ADR authorizes no implementation or deployment. Acceptance depends on the
[experiment gates](../../specs/024-gstreamer-webrtc-research/research.md#experiment-plan-and-decision-gates).

## Component boundaries

| Component | Responsibility |
| --- | --- |
| Platform capture adapter | Bounded texture/pixel-buffer leases, geometry, timestamps and capture recovery; explicit GPU conversion/readback accounting |
| Optional host media adapter | GStreamer lifecycle, encoder selection, bounded queues, bitrate feedback, keyframe recovery, and metrics |
| Session authorization/control | Pairing and Access policy, Noise-bound SDP/ICE/DTLS identity, roles, expiry, revocation and fixed destinations |
| Browser media adapter | Peer connection, negotiated codec, video playback, lifecycle handling and available stats |
| Shared input semantics | Validated key/button ownership, pointer coordinates/relative movement, session epochs and release-all; no dependency on RFB wire framing |
| Native video adapter, later | Decode and wgpu presentation interop, independent of protocol and input processing |

These are logical boundaries, not new crates created by this task. Keep platform
capture ownership separate from media plumbing. A standalone GStreamer capture
source may establish feasibility; it cannot establish parity with TopVNC's
monitor recovery or Windows service behavior.

Use direct ICE connectivity where policy permits and an independently configured
TURN service where needed. The existing encrypted WebSocket route can be
extended for signaling through a versioned control protocol. Bind the complete
negotiation to the paired session before accepting input. Closing/revoking that
session must terminate media/control even if a peer connection still works.

## Options considered

| Option | Complexity and cost | Advantages | Disadvantages and disposition |
| --- | --- | --- | --- |
| Keep RFB only | Lowest new engineering and operating cost | Current peer compatibility, incremental updates, existing identity | Full-motion bandwidth and ordered-stream stalls remain; retain as baseline |
| GStreamer + WebRTC | Moderate/high integration; native runtime and potential relay egress | Browser media stack, encoder ecosystem and existing adaptation machinery | Packaging, GPU interop and mobile latency need measurement; first experiment |
| Direct hardware APIs + separate WebRTC stack | Highest platform ownership; similar relay cost | Precise capture/encoder controls, potentially smaller selected runtime | More API, packetization/feedback and maintenance work; investigate after measured GStreamer blocker |
| Sunshine/Moonlight | Lowest effort as separate applications; high integration cost | Established game-streaming reference and native/mobile clients | Different protocol/browser contract and GPL reuse decision; external benchmark |
| WebTransport/WebCodecs | High custom protocol/server effort | Streams/datagrams and explicit browser codec operations | Own media adaptation, recovery and playout; deployment and device matrix need proof; defer |

The options and capabilities above are substantiated in the
[research comparison](../../specs/024-gstreamer-webrtc-research/research.md#comparison);
relative effort and suitability are engineering judgments.

## Trade-offs and consequences

- A second optional session path increases packaging, testing, signaling and
  support complexity while preserving existing VNC interoperability.
- High-level GStreamer adaptation saves initial implementation work but may
  constrain encoder control; moving down to webrtcbin transfers more policy
  ownership to TopVNC.
- Raw-frame replacement helps bound latency; arbitrary encoded-frame dropping
  can break decoder references. Recovery requires a decodable boundary.
- H.264's broad interoperability does not guarantee hardware acceleration,
  120 Hz or desktop text quality. Codec/profile and GPU paths must be negotiated
  and observed on physical devices.
- TURN adds relay operation and traffic costs. Browser-delivered code retains
  the current provider trust limit; encrypted media does not authorize control.
- Native video presentation and macOS VideoToolbox are follow-up adapters with
  their own validation. Windows results do not establish their support.

## Action items before acceptance

1. [ ] Specify and pin the initial GPU, browser/phone matrix, GStreamer runtime,
   Rust bindings/plugins, quality rubric and numerical experiment gates.
2. [ ] Build an isolated media-only hardware H.264 feasibility experiment and
   record GPU copies, queue depth and actual encoder/decode paths.
3. [ ] Specify and test paired-session signaling, input authorization, bounded
   payloads, expiry/replay rejection and host-side stuck-key recovery.
4. [ ] Compare matched workloads and network conditions against RFB; use
   Sunshine/Moonlight as a separately labeled native reference.
5. [ ] Run soak, background/reconnect, monitor-change and forced-TURN tests;
   review relay costs and a clean-machine package/plugin inventory.
6. [ ] Accept, revise or reject this ADR from measured evidence. Create the
   implementation feature spec before changing production behavior.

No action items are implemented by this documentation task.
