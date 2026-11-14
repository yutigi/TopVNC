# Reference-matched browser UI

## Scope

Replace the browser viewer's navy/teal presentation with the accepted mobile
reference in `design/topvnc-mobile-wireframe/`.
Use its paper/olive colors, rounded controls, typography, spacing, pinned-key
row, modifier/arrow groups, typing keyboard and fixed mode dock. Preserve live
session behavior, authentication, encryption and input ownership.

Adapt the same visual language to desktop browsers: a wide remote display,
compact session header and a remote display filling the space beneath it, without
an input-control rail, dock, custom keyboard, trackpad or FPS touch overlay. Publish a separate
interactive desktop design Site. The existing mobile reference Site and its
tracked source are read-only for this work.

## Acceptance criteria

- Work is isolated on `feat/web-reference-ui`.
- Mobile Keys matches the reference structure: four pinned shortcuts, a 2×3
  key/modifier group beside inverted-T arrows, a hint row and a four-slot dock.
- Type matches the reference source switch, compose/Insert row, staggered
  keyboard, globe/symbol/space/backspace/return row and language editor entry.
- Pointer uses the reference trackpad and separate mouse buttons. FPS controls
  overlay the remote view while keeping the dock and session actions reachable.
- Reference palette values come directly from the accepted source; live remote
  framebuffer pixels are never recolored. Authentication/security information
  remains visible and accurate.
- Desktop uses the same palette and a full-width remote view beneath the header.
  At widths of at least 900 CSS pixels with a fine primary pointer and hover,
  hide all touch input controls regardless of saved mode, FPS preset or deck
  placement. Keep physical keyboard/mouse routing, fit/fullscreen, settings and
  visible authentication/encryption state. Release held input on layout changes.
- Coarse-pointer devices retain the mobile controls, including narrow landscape
  phones and wide touch tablets. Mobile deck placement remains configurable.
- The visible header Settings action opens every session setting: Display,
  Served size, Picture quality, fullscreen, debug/Stats, security and Disconnect.
  Production controls follow advertised host capabilities and show explicit
  unavailable states. The desktop prototype provides clearly labeled simulated
  controls, capability examples and diagnostic samples without a live host.
  Both panels support keyboard focus, Escape, and scrolling on small viewports.
- Existing custom keys, language preferences, native IME, hardware keyboard,
  independent input holds, and lifecycle releases continue working.
- A new desktop Site has its own project ID and URL. No publish, source push,
  metadata or access mutation targets the original mobile Site.
- Verify production/offline UI at narrow portrait, mobile landscape and desktop
  widths, exercise input routing, and run the workspace checks in AGENTS.md.

## Visual fidelity correction

Compare against the immutable mobile wireframe source, including actual font
weights, normal line height, button padding, tint, keyboard geometry, spacing
and dock colors. Add reference-derived screenshot goldens for Keys, Type and
Pointer controls at portrait and landscape sizes. Baselines must be generated
from the reference, never approved from the implementation under test. Exclude
the simulated framebuffer, prototype bezel/annotations and truthful production
security copy from pixel comparisons. Keep accessible touch targets,
host capability settings, desktop behavior and input routing intact. Record the
font provenance and verify reference/source hashes so drift is explicit.

## Follow-up control corrections

- Remove the Pointer deck's extra screen-input label and mode toggle row.
- Expose the existing landscape deck placement in Session settings; changes
  apply immediately, persist on this device, and preserve the portrait layout.
- Match the accepted Type composition controls and stacked landscape rows.
  Keep text insertion, native IME, shortcuts, and every keyboard row usable.
- In a landscape side deck, expand the trackpad to use the available height.
  Keep both click buttons fully visible directly below it, above the mode dock.

## Verification limits

Screenshots use a clearly labeled offline session fixture. They do not establish
physical-device, Safari, live game, latency or remote-session support. Production
Cloudflare deployment is outside this task; the new design Site is published.
