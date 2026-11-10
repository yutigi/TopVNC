# GStreamer/WebRTC for TopVNC

**Research date:** 2026-11-11

**Code baseline:** main at 1e6a814f9254f0a692be0e6d8fdbdab33b952d15

**Scope:** Windows host to browser/mobile first; native and macOS implications.

## Recommendation

Prototype an optional GStreamer/WebRTC video session alongside RFB. Start with
webrtcsink to test hardware H.264, browser playback, and bandwidth adaptation.
Use webrtcbin if the high-level element cannot meet the required encoder,
identity, or input contracts. Retain RFB as an explicitly selected compatibility
and desktop-quality path.

This is an engineering recommendation inferred from the sources and code below.
No GStreamer pipeline was run, no comparative performance was measured, and no
new platform or encoding support is claimed. The [ADR](../../docs/adr/0001-browser-gaming-media.md)
is **Proposed**, not an accepted implementation decision.

The useful experiment is whether video encoding plus WebRTC improves gaming
under constrained or lossy networks enough to justify packaging and operating a
second media path. GStreamer is a media framework; H.264 is a codec; WebRTC and
WebTransport supply different transport building blocks. Direct hardware APIs
can also feed WebRTC. These choices overlap rather than forming five equivalent
drop-in replacements.

## What the supplied talk establishes

The [conference page][S01] identifies Matthew Waters of Centricular, creation
date 2017-10-21, and a presentation about WebRTC concepts, native implementation,
peer-to-peer streaming, gateways, and streaming servers. The retrieved page
exposes transcript/slide UI labels, but no usable transcript or slides were
retrieved. This research does not attribute detailed technical claims or timings
to the recording. The talk is historical context; current element documentation
supplies the capability evidence.

## Existing TopVNC baseline

Repository observations at the pinned revision:

| Boundary | Evidence and implication |
| --- | --- |
| Capture | [Windows capture](../../src/windows_server.rs), OutputCapture::next_frame/copy_to_staging: Desktop Duplication texture to CPU-readable staging, Map, then changed-region copies into the shared image. A hardware encoder attached after that image still pays GPU readback and potentially another upload. |
| RFB | [Shared crate](../../crates/topvnc-rfb/src/lib.rs), [host/session library](../../src/lib.rs), and [Tight encoder](../../src/tight.rs): incremental Raw/Zlib/Tight decoding and JPEG encoding. These remain useful for existing VNC peers and desktop content. |
| Native presentation | [Mailbox](../../src/presentation.rs) and [render worker](../../src/session_render.rs): bounded handoff of coherent images, independent of input. Video must preserve this separation rather than run on the input/UI thread. |
| Browser | [Session](../../crates/topvnc-web/src/session.rs) and [frame state](../../crates/topvnc-web/src/frame.rs): Rust/WASM consumes encrypted RFB and applies decoded damage to RGBA state. Browser WebRTC video would instead use RTCPeerConnection and a video element. |
| Network/security | [Gateway](../../crates/topvnc-web-gateway/src/lib.rs) and [security model](../../docs/cloudflare-gateway.md#security-model): Access checks plus browser-to-host Noise with QR-pinned host identity and enrolled browser keys. The existing loopback RFB leg is an explicit, constrained plaintext integration. |
| macOS | [Capture host](../../src/macos_server.rs): ScreenCaptureKit retains CVPixelBuffer/IOSurface, then reads pixels for RFB. A future VideoToolbox path should preserve buffer lifetime instead of requiring that CPU representation. |

The [existing loopback benchmark](../011-smooth-gaming/spec.md#networkcodec-boundary)
reports 1080p/120 Hz Tight quality 6 at 253.1 Mbit/s, with 4.496 ms mean and
5.620 ms p95 publish-to-decode latency. That historical synthetic run excludes
capture, physical input, GPU presentation, and display. It illustrates a
bandwidth concern, not measured browser latency or evidence that WebRTC is faster.
No old benchmark was rerun during this task.

## Comparison

The assessments below combine documented properties with TopVNC integration
judgment. Complexity is relative to the current code, not a measured score.

| Approach | Browser/mobile fit | Latency and quality considerations | Integration and operations | Recommended role |
| --- | --- | --- | --- | --- |
| Existing RFB/Tight over TLS/TCP or Noise/WebSocket | Current experimental browser implementation; retains incremental framebuffer semantics | JPEG rectangles lack video interframe compression; reliable ordered transport can delay newer bytes behind lost data. Raw/Zlib remain lossless choices. Current flow control bounds application backlog. | Smallest incremental effort; current gateway, identity, and compatibility already exist | Baseline and explicit compatibility option |
| GStreamer + WebRTC | Browser-native peer connection and video decode; host has native dependencies | Hardware encode plus interframe compression may save bandwidth; capture conversion, encoder buffering, jitter buffer, and browser compositor can add delay. H.264 4:2:0 needs text-quality checks. | High-level media plumbing available; custom authenticated signaling, input, ICE/TURN, and deployment still needed | First video experiment |
| Direct Media Foundation/NVENC/VideoToolbox + a WebRTC stack | Same browser contract if the separate stack interoperates | Best opportunity for precise texture ownership and encoder controls; API choice alone says nothing about network or display latency | Highest platform-specific ownership; encoder adapters, packetization, feedback integration, recovery, and packaging must be designed with the chosen stack | Second experiment if GStreamer misses measured copy/control goals |
| Sunshine + Moonlight | Native/mobile clients exist; ordinary browser playback needs a bridge/new client | Game-streaming reference for hardware encoding, pacing, and controls; no TopVNC comparison measured | Existing application/protocol ecosystem; porting code brings GPL and native dependencies | External benchmark and design reference; separate native-app option |
| WebTransport + WebCodecs + hardware encoder | Browser API support and exact codec/config must be checked per device | Independent streams/datagrams avoid a single ordered byte stream, but application must implement video recovery, scheduling, feedback, and playout | Public HTTP/3 endpoint required; largest custom browser media protocol effort | Defer unless WebRTC exposes a measured blocker |

Source basis: GStreamer [webrtcbin][S02], [rswebrtc][S03] and [sink][S04];
[Media Foundation][S08], [NVENC][S09], [VideoToolbox][S10];
[Sunshine][S11], [Moonlight core][S12], [legacy Chrome client][S13];
[WebTransport][S14] and [WebCodecs][S15]. RFB observations come from the
repository links above.

### GStreamer: choose the level of ownership deliberately

webrtcbin exposes peer-connection-style SDP, ICE, RTP tracks and DataChannels;
the application supplies the surrounding pipeline and signaling. It does not
choose an end-to-end desktop latency policy for TopVNC. [S02]

webrtcsink adds a signaller interface, per-consumer pipelines, congestion-driven
encoder bitrate adaptation, retransmission and FEC facilities. Its documented
internally encoded path does not share encoding among consumers. This supports
a one-viewer experiment, but makes multi-viewer GPU session limits and cost an
explicit later question. [S03]

The sink accepts raw GPU memory types including D3D11Memory and also encoded
H.264 and other video caps. Its encoder-setup and peer-connection hooks are
useful escape hatches. Encoded input is not proof that the sink can regulate an
external encoder: define and test the bitrate/keyframe feedback contract before
using that mode. Do not put a second independent bitrate controller over the
sink's internal one. [S04]

The proposed first experiment uses raw input and a pinned, inspected hardware
encoder selection. It reports the actual element, GPU adapter, profile and
memory caps. If hardware is absent, fail clearly or offer an explicit software
test mode; record it as a separate result. Disable generic navigation/control
forwarding until TopVNC's authorization and input adapter are in place.

### GPU path and codec choices

GStreamer's screen-capture source advertises BGRA D3D11Memory. mfh264enc
advertises NV12 input, including D3D11Memory, and exposes D3D11-awareness and
low-latency properties. nvd3d11h264enc advertises a D3D11 input path for supported
NVIDIA hardware. Element availability and caps are capabilities, not evidence
that TopVNC's particular GPU, driver, resolution and adapter pairing work.
[S05] [S06] [S07]

Test two capture paths separately:

1. A standalone GStreamer capture source, to isolate media feasibility quickly.
   It does not exercise TopVNC's existing capture recovery or service behavior.
2. A future adapter for TopVNC-owned textures, retaining frames until GPU work
   completes and converting BGRA to an encoder-compatible format on the GPU.

For both, instrument texture copies, synchronization stalls, CPU readback,
format conversion and GPU identity. A GPU-resident path can still require GPU
copies. Do not claim zero-copy from a pipeline diagram or advertised caps.
TopVNC's service/secure-desktop and monitor-change behavior must be separately
validated before changing its capture ownership.

Media Foundation offers low-latency codec controls, but hardware MFT selection
and supported properties must be queried. NVENC documents low/ultra-low latency
tuning and hardware capability queries; neither API replaces a media transport.
[S08] [S09]

Use negotiated H.264 Constrained Baseline as the initial interoperability
candidate; also test VP8 if available under an explicit software/hardware policy.
RFC 7742 requires both in WebRTC browsers, but does not guarantee hardware
acceleration, a desired resolution, or 120 Hz. Verify SDP profile-level-id and
packetization-mode plus actual decode behavior. Defer HEVC, AV1, HDR and 4:4:4
until the initial device matrix passes. [S16]

The common 4:2:0 video path can blur colored small text. Compare moving gameplay
and a separate text/UI workload; use RFB Raw/Zlib as a lossless reference.
Temporal compression and rate control also change quality during motion and
recovery, so equal bitrate is not equal quality. [S16]

For macOS, preserve CVPixelBuffer ownership for a potential VideoToolbox adapter.
Apple's low-latency rate-control option disables reordering/lookahead but selects
High profiles. That is a concrete negotiation issue for a Constrained Baseline
experiment: query available properties, configure another compatible mode or
negotiate a mutually supported profile, and test it. Windows success does not
establish macOS support. [S10]

### Sunshine/Moonlight: useful benchmark, different browser contract

Sunshine documents Windows hardware encoders and a browser UI for configuration
and pairing. That UI is not evidence of browser video playback. Moonlight's
shared C library implements GameStream; its older Chrome client depends on
deprecated NaCl. A native C client compiled to WASM does not acquire unrestricted
UDP/TCP sockets in an ordinary browser. A WebRTC/WebTransport bridge would add a
new transport, input mapping, identity boundary, and possibly decode/re-encode.
[S11] [S12] [S13]

Use Sunshine/Moonlight externally on matched hardware to benchmark achievable
native game streaming. If the immediate goal becomes native mobile streaming,
running those applications separately deserves consideration before building
another protocol stack. Copying or linking their GPL-3.0 code needs a separate
distribution decision; documentation review is not code reuse approval.
[S11] [S12]

### WebTransport/WebCodecs: viable building blocks, more owned policy

The fetched MDN WebTransport page marks the API Baseline 2026, newly available
since March 2026. Do not repeat older blanket claims that it is unavailable in
Safari; also do not infer coverage of older phones or every API subfeature.
Probe the exact deployed browsers and datagram support. [S14]

WebCodecs exposes codec operations, not a complete streaming player or wire
format. Check VideoDecoder.isConfigSupported for each exact codec/config and
verify real decode; a positive configuration probe is not a hardware-use or
performance guarantee. [S15] [S17]

TopVNC would own frame/chunk IDs, bounded fragment assembly, maximum datagram
size handling, expiry, keyframe requests, congestion-informed encoder rates,
decoder queue limits and presentation scheduling. Streams provide reliability
and QUIC provides congestion control, but neither supplies a desktop-specific
media adaptation policy. A separate public HTTP/3 service and authentication
model are necessary. These costs outweigh the current evidence for choosing it
before trying WebRTC.

## Proposed topology and trust boundaries

~~~mermaid
flowchart LR
    B["Browser: controls + RTCPeerConnection"]
    W["Existing Worker / Tunnel"]
    H["Host: paired session authorization"]
    C["Windows capture adapter"]
    G["GStreamer media adapter"]
    T["TURN relay when needed"]
    I["Host input validation + ownership"]
    B <-->|"authenticated signaling in Noise / WebSocket"| W
    W <-->|"ciphertext"| H
    H -->|"authorize and bind peer"| G
    H -->|"authorize and revoke input"| I
    C -->|"bounded texture leases"| G
    G <-->|"direct ICE / DTLS-SRTP"| B
    G <-->|"encrypted media relay"| T
    T <-->|"encrypted media relay"| B
    B -->|"authorized DataChannel input"| I
~~~

All boxes and new routes above are proposed. The current /ws contract carries
RFB bytes; it does not already accept SDP. Add a versioned, bounded session
control protocol in a separately specified change. Keep the Noise record layer
on the reliable signaling stream. Do not reuse its ordered-record assumptions
over unreliable media packets.

Cloudflare documents WebSocket support for Tunnel and specific published
application protocols. The existing HTTP/WebSocket route is not a public ICE,
TURN or WebTransport endpoint. QUIC between cloudflared and Cloudflare does not
give the browser arbitrary UDP access to the host. Private network client
routing is a different product topology. [S18] [S19]

ICE uses connectivity checks; STUN helps discover candidates; TURN supplies
relaying where direct connectivity fails. Test direct UDP, relayed UDP and
TURN/TLS on restrictive networks. TURN/TCP or TLS can reintroduce delay from
reliable transport. Cloudflare offers a separate managed TURN service; do not
assume the current Worker/Zero Trust plan includes its use. [S20] [S21]

TURN relay traffic remains protected between the two WebRTC endpoints. An SFU
is a different media endpoint/trust design; introducing one requires another
security decision. Estimate relay bytes per session from measured traffic:
20 Mbit/s for one hour is about 9 decimal GB in one media direction before
overhead. Obtain current provider billing rules for actual paths before choosing
hosting; this task neither selects a plan nor provisions resources.

DTLS-SRTP protects media and SCTP/DTLS protects DataChannels, but encryption does
not authorize desktop control. [S22] Preserve the current independent host and
browser identity: send the complete offer/answer, DTLS fingerprints, ICE
credentials and candidates through the enrolled Noise session; bind them to a
fresh session ID, role and expiry. Accept media/input only for that authorized
peer. Reject replay, changed bindings and unauthorized renegotiation. Revoke
media and input on authorization expiry or session closure. Cap signaling sizes,
candidate counts, connection attempts, resolutions and peer count before work or
allocation.

This binding is a proposed application design, not an implemented or audited
protocol. Access alone must not replace host pairing. TURN credentials must be
short-lived and issued after authorization; no TURN shared secret belongs in
browser code. Direct ICE also changes network/IP exposure and host firewall
behavior, so offer a relay-only policy where required. Provider-served browser
code retains the existing documented trust limit.

## Backpressure, recovery and input contracts

These are proposed requirements for the future experiment:

- Keep at most one pending raw frame before encode, replacing stale raw work;
  account separately for capture leases, converter pools and encoder in-flight
  surfaces. Start with one viewer and 1080p/60; cap every pool and dimension.
- Configure each explicit GStreamer queue. Defaults allow up to 200 buffers,
  10 MB or one second, whichever is reached first. A raw-frame queue with
  max-size-buffers=1, max-size-bytes=0, max-size-time=0 and downstream leakage
  illustrates a latest-pending-frame policy; it does not bound internal encoder,
  RTP or browser queues. Inspect and measure each of those separately. [S23]
- Do not drop arbitrary encoded P/B frames and expect later frames to remain
  decodable. Prefer dropping raw work before encoding. Under a stale or damaged
  encoded dependency chain, discard safely, request/force a new keyframe,
  reset codec state as required, and resume only at a decodable boundary.
  This differs from applying incremental RFB rectangles: preserve accumulated
  framebuffer damage even when skipping presentation.
- Bound receiver latency and sender backlog with explicit recovery/close
  deadlines. Lower bitrate, capture rate or served size under policy control;
  expose the change. Browser-managed jitter/decode buffers may expose only
  hints or limited controls; measure their behavior and close/recover when the
  age budget is exceeded. Do not solve congestion by accumulating old frames.
- Use a reliable ordered small-message input channel for key/button transitions,
  wheel events, control and release-all. Coalesce absolute pointer positions.
  Unordered/partially reliable movement needs sequence numbers and epochs.
  Relative motion additionally needs cumulative counters or an explicit loss
  policy; dropping deltas changes the game's motion.
- Separate movement and reliable transitions with barriers/epochs where ordering
  matters (for example, button-down at a position). Validate scan codes, button
  masks, bounds, rates and permissions before platform injection. The sink's
  generic navigation protocol does not supply TopVNC's relative-pointer and
  multi-source key-ownership semantics.
- Enforce input-channel bufferedAmount high-water limits; do not send large
  clipboard payloads ahead of interactive traffic. DataChannels buffer messages,
  and large messages can delay others. Add chunking, quotas and cancellation
  for any later clipboard feature. [S24]
- Release host-held input on close, revoked authorization, focus/visibility loss
  or host-side input lease expiry. Use a host watchdog because a suspended phone
  cannot be relied on to send key-up. A new connection gets a new epoch and starts
  with no held keys; never replay buffered input after reconnect.

## Browser, native and distribution implications

A browser experiment should use a video element with playsinline and an
explicit start gesture; account for autoplay restrictions and call play() error
handling. Keep existing touch controls and geometry mapping independent from
video decode. Audio needs its own capture, permission, synchronization and
autoplay scope before being added. [S25]

Background tabs can throttle animation/timers; real-time connections can receive
exceptions, so backgrounding does not universally mean immediate disconnect.
Treat lock, suspension, Wi-Fi/cellular changes and resume as device tests.
On resume, expire input, reconnect/restart ICE as policy permits, and require a
fresh decodable image rather than playing old queued video. [S26]

A native Rust viewer would need a separate video decode/presentation adapter
and texture interop with wgpu, or a clearly measured CPU-copy fallback.
GStreamer video surfaces cannot simply be assumed to be wgpu textures. Keep
RFB protocol and framebuffer crates free of GStreamer, GLib and platform UI
dependencies. A host-only GStreamer prototype leaves browser decode native to
the browser and minimizes the first experiment's native-viewer scope.

Rust bindings still depend on the native GStreamer runtime. The bindings and
Rust plugin collection have independent release schedules; record the complete
compatible set, rather than assuming a Rust crate version pins every plugin.
Windows deployment must supply the runtime, selected plugins and their
dependencies; build-time development files alone are insufficient. [S27] [S28]

Prepare a minimal plugin inventory and clean-machine installer test. Include
webrtcbin, ICE/DTLS/SRTP dependencies, the selected encoder/converter and
rswebrtc where used. Check each shipped component's notices, source/relink
requirements and codec terms. GStreamer's LGPL does not determine every
plugin/codec's distribution obligations; automatic selection of an installed
software encoder could also change the inventory. This is a packaging action
item, not a legal clearance. [S29]

## Experiment plan and decision gates

The following thresholds are **proposed experiment criteria**, not measured
results or new product promises. Freeze them and the quality rubric in the
implementation spec before collecting comparison data.

| Stage | Work and evidence | Gate |
| --- | --- | --- |
| 0: Reproducible baseline | Pin commit, OS/browser builds, phone models, GPU/driver, display Hz, capture size, codec settings, network shaper and power state. Inventory installed elements/caps and actual hardware encode/decode. | Missing capabilities are reported explicitly; no hidden software or transport fallback. |
| 1: Media-only feasibility | Windows normal desktop to Edge/Chrome and physical iPhone Safari; first 1080p/60 H.264, then Android Chrome. Test webrtcsink raw-input hardware path and text/motion quality. No unauthenticated input injection. | Every declared test pairing negotiates a decodable profile and resolution; memory/copy path and queues are observable. Unsupported pairs stay unsupported. |
| 2: Authorized interaction | Add Noise-bound signaling and a versioned input adapter; direct and forced-TURN runs; expiry, wrong host/browser identity, replay, oversize messages, blocked UDP and disconnect while keys held. | Negative cases reject before capture/control; fail-closed behavior; host releases held input within a proposed 1 s watchdog deadline. |
| 3: Fair comparison | Compare RFB Tight q3/q6/q9, GStreamer hardware H.264, a selected direct API with the same codec/WebRTC settings where feasible, and Sunshine/Moonlight as an external native reference. | At matched accepted visual quality, seek at least 30% lower median wire bandwidth than the selected RFB baseline on motion workloads. p95 physical input-to-visible latency must be no worse than that baseline plus one target frame period (16.7 ms at 60 Hz). |
| 4: Stability and recovery | Two-hour soak; repeat background/lock/resume and Wi-Fi/cellular changes; monitor unplug/resize, GPU reset, slow receiver and TURN loss; later 120 Hz where hardware permits. | No unbounded queue growth; after warmup the final 30-minute median resident memory must not exceed the first stable 30-minute median by more than 10% or 32 MiB (larger allowance), with no sustained growth trend. Proposed recovery-to-decodable-frame p95 under 2 s after connectivity returns. |
| 5: Selection | Review latency distributions, pacing, CPU/GPU/memory, quality, relay share, operational cost and installer footprint together. | Choose webrtcsink only if all applicable gates pass; try webrtcbin/direct encoding if a measured controllability or copy issue is the blocker. Keep WebTransport deferred unless a concrete WebRTC limitation justifies owning more protocol. |

For stage 3 use the same source scene, served resolution, frame-rate target,
capture hardware and physical client. Separate desktop text, slow game motion
and fast camera pans. Test controlled LAN, constrained Wi-Fi-like and mobile-like
links; start with reproducible 100/20/10 Mbit/s caps, 0/30/80 ms RTT and
0/1/3% loss, varying one factor before selected combined cases. Label these
synthetic profiles, not representations of every real network.

Alternate test order; use at least five runs per condition with warmup and at
least 60 s recorded. Show per-run values and p50/p95/p99, frame interval
distribution, stalls over 100 ms, and capture/encode/decode/present drop counts.
For 60 Hz motion, propose at least 95% of target distinct frames delivered and
p95 presentation interval within two frame periods; count loss-induced freezes
rather than hiding them in an FPS average. Quality must pass a predefined
text-legibility and motion-artifact review; retain reproducible screenshots or
source material and disclose differences from RFB quality settings.

Measure host and client CPU time, GPU encode/3D use, peak and steady memory,
wire bytes (including relay/repair overhead), and queue age. Log frame IDs and
monotonic stage timestamps; do not subtract unsynchronized host/client clocks.
Use browser getStats for available loss, RTT, decode and jitter-buffer
diagnostics; record absent fields instead of fabricating them. Browser
framesDecoded is not proof that a frame was physically displayed. [S30]

Use a high-speed camera or instrumented physical input/light-response setup for
input-to-visible latency, recording camera sampling uncertainty and display Hz.
Keep synthetic encode/transport/decode timings separate from physical results.
Sunshine/Moonlight on a native client is an external reference, not a controlled
browser transport comparison; disclose the changed client and decoder.

No WebTransport implementation is required to select the first experiment.
If later evaluated, reuse the same source/codec/quality criteria and publish
its additional server, assembly, recovery and browser-compatibility costs.

## Open questions for an implementation task

- Which Windows GPU/driver and physical iPhone/Android versions are available
  for the first reproducible runs? Is GPU-less software mode required?
- Can webrtcsink's pinned release enforce the desired encoder and expose the
  required authorized DataChannel hooks without forking it?
- Can the existing capture device safely share texture ownership with the chosen
  converter/encoder across monitor changes and service agent restarts?
- What is the measured iOS decode/jitter-buffer/presentation floor at 60 Hz,
  and is mobile 120 Hz achievable on the actual devices?
- Which direct/TURN paths work through the intended restrictive networks,
  and what relay usage, egress cost and IP-exposure policy are acceptable?
- What distribution license will TopVNC use, and which exact plugin/codec set
  can its installers ship? No component reuse decision is made here.
- Are audio, gamepads, secure desktop, multiple displays or multiple viewers
  required for the first product feature? They were excluded from this R&D.

## Evidence register

All links below were returned by search/fetch during this research. Most
documentation is rolling and unversioned: research access on 2026-11-11 does
not establish a tested release combination. Record runtime versions in stage 0.
Search/fetch supplied extracts, not executable compatibility evidence.

| ID | Primary source | Evidence used |
| --- | --- | --- |
| S01 | [Conference page][S01] | Matthew Waters; 2017-10-21; abstract only |
| S02 | [webrtcbin][S02] | Peer connection, SDP, tracks, DataChannels |
| S03 | [rswebrtc overview][S03] | Signaller, per-consumer encode, congestion, FEC/repair |
| S04 | [webrtcsink API][S04] | Input caps, hooks, bitrate controls |
| S05 | [d3d11screencapturesrc][S05] | DXGI capture, BGRA D3D11Memory |
| S06 | [mfh264enc][S06] | NV12/D3D11 input, hardware classification and controls |
| S07 | [nvd3d11h264enc][S07] | NVIDIA D3D11 encoder input |
| S08 | [Microsoft H.264 encoder][S08] | Low-latency controls and hardware caveats |
| S09 | [NVIDIA NVENC SDK 13.1 guide][S09] | Tuning, input resources and capability queries; not a tested SDK pin |
| S10 | [Apple low-latency rate control][S10] | No reordering/lookahead; High-profile restriction |
| S11 | [Sunshine repository][S11] | Hardware encoders, pairing UI, GPL-3.0 |
| S12 | [Moonlight common C][S12] | GameStream client core and GPL-3.0 |
| S13 | [Moonlight Chrome][S13] | Legacy NaCl dependency |
| S14 | [MDN WebTransport API][S14] | Current availability, HTTP/3 streams/datagrams |
| S15 | [MDN WebCodecs][S15] | Codec operations and configuration limits |
| S16 | [RFC 7742, March 2016][S16] | Mandatory codecs and screen-content chroma caveat |
| S17 | [MDN decoder configuration probe][S17] | Exact configuration support checking |
| S18 | [Cloudflare Tunnel FAQ][S18] | WebSocket support |
| S19 | [Cloudflare published protocols][S19] | Public HTTP/TCP route model |
| S20 | [RFC 8835, January 2021][S20] | ICE/STUN/TURN and TCP/TLS fallback |
| S21 | [Cloudflare TURN][S21] | Separate relay service; no plan selected |
| S22 | [RFC 8827, January 2021][S22] | Fingerprint binding and signaling trust; DTLS/SRTP |
| S23 | [GStreamer queue][S23] | Limits, blocking and leakage |
| S24 | [MDN DataChannels][S24] | Buffering and message-size interference |
| S25 | [MDN video element][S25] | Inline playback and autoplay |
| S26 | [MDN visibility][S26] | Background timer/animation policies and exceptions |
| S27 | [GStreamer Rust bindings mirror][S27] | Native dependencies and independent release cycles |
| S28 | [GStreamer Windows installation][S28] | Runtime/development packages and plugin discovery |
| S29 | [GStreamer licensing advisory][S29] | LGPL plus plugin/codec caveats |
| S30 | [W3C WebRTC statistics][S30] | Field definitions and decoded versus rendered counts |

[S01]: https://gstconf.ubicast.tv/videos/gstreamer-webrtc/
[S02]: https://gstreamer.freedesktop.org/documentation/webrtc/
[S03]: https://gstreamer.freedesktop.org/documentation/rswebrtc/index.html
[S04]: https://gstreamer.freedesktop.org/documentation/rswebrtc/webrtcsink.html
[S05]: https://gstreamer.freedesktop.org/documentation/d3d11/d3d11screencapturesrc.html
[S06]: https://gstreamer.freedesktop.org/documentation/mediafoundation/mfh264enc.html
[S07]: https://gstreamer.freedesktop.org/documentation/nvcodec/nvd3d11h264enc.html
[S08]: https://learn.microsoft.com/en-us/windows/win32/medfound/h-264-video-encoder
[S09]: https://docs.nvidia.com/video-technologies/video-codec-sdk/13.1/nvenc-video-encoder-api-prog-guide/index.html
[S10]: https://developer.apple.com/documentation/videotoolbox/kvtvideoencoderspecification_enablelowlatencyratecontrol
[S11]: https://github.com/LizardByte/Sunshine
[S12]: https://github.com/moonlight-stream/moonlight-common-c
[S13]: https://github.com/moonlight-stream/moonlight-chrome
[S14]: https://developer.mozilla.org/en-US/docs/Web/API/WebTransport_API
[S15]: https://developer.mozilla.org/en-US/docs/Web/API/WebCodecs_API
[S16]: https://www.rfc-editor.org/rfc/rfc7742.html
[S17]: https://developer.mozilla.org/en-US/docs/Web/API/VideoDecoder/isConfigSupported
[S18]: https://developers.cloudflare.com/cloudflare-one/faq/cloudflare-tunnels-faq
[S19]: https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/routing-to-tunnel/protocols
[S20]: https://www.rfc-editor.org/rfc/rfc8835
[S21]: https://developers.cloudflare.com/realtime/turn
[S22]: https://www.rfc-editor.org/rfc/rfc8827.html
[S23]: https://gstreamer.freedesktop.org/documentation/coreelements/queue.html
[S24]: https://developer.mozilla.org/en-US/docs/Web/API/WebRTC_API/Using_data_channels
[S25]: https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/video
[S26]: https://developer.mozilla.org/en-US/docs/Web/API/Page_Visibility_API
[S27]: https://github.com/GStreamer/gstreamer-rs
[S28]: https://gstreamer.freedesktop.org/documentation/installing/on-windows.html
[S29]: https://gstreamer.freedesktop.org/documentation/application-development/appendix/licensing.html
[S30]: https://www.w3.org/TR/webrtc-stats/
