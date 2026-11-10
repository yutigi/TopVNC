# TopVNC desktop web design

Separate desktop companion to the accepted [mobile reference](https://topvnc-mobile-wireframe-nov26.example.chatgpt.site/).
The original Site and source are preserved unchanged.

This is an interactive design prototype with a simulated remote editor and FPS
scene. It never connects to a host or accepts remote-session credentials.

Use Desktop or Compact laptop to compare workspace density. The remote view
fills the session body with no input rail, dock or touch overlay. Click the
simulated editor to type using your physical keyboard. Header actions provide
Fit, full viewport and session settings. Full viewport removes the design-lab frame. The physical Escape key closes
dialogs or exits full-viewport mode. The palette and fonts are copied from the
mobile reference; fonts are bundled locally with their licenses.

The production implementation lives in `web/`. This design's source lives in
`dist/`; `.sites-runtime/` is an ignored, separate publication checkout. The
hosting manifest identifies this desktop Site only.

Settings includes simulated display selection, served size (25–100%), picture
quality, full-viewport toggle, Debug / Stats, security details and disconnect /
reconnect. The host-capability example switches sizing/display controls between
offered and unavailable. Diagnostic values are explicitly labeled examples.
The settings panel scrolls on small screens and supports focus trapping, Escape
and focus return. No control contacts a host or collects credentials.
