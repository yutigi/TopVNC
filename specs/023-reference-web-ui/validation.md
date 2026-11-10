# Validation — 2026-11-09

## Implementation

Branch: `feat/web-reference-ui`.

The production browser UI uses the accepted reference's paper/ink/olive tokens,
local DM Sans and IBM Plex Mono fonts, session header, pinned shortcuts,
modifier/arrow grouping, custom typing keyboard, globe key, pointer pad and dock.
Desktop uses a full-width remote view beneath the header with no input rail,
dock, custom keyboard, trackpad or FPS touch overlay. Mobile FPS controls overlay
the framebuffer. Fonts
are included in the build with their OFL licenses; the session adds no external
font requests. Protocol, cryptography and transport code are unchanged.

Pinned preferences are restricted to four allowlisted shortcuts. Tests cover
real chord routing, edits, reloads and malformed/oversized saved values. All Fn
buttons update their expanded state; replacing the Fn slot cannot break close
or reconnect. The dynamic keyboard Shift shares the existing modifier state.

## Checks

- `cargo fmt --all`, `cargo test --workspace`, and
  `cargo clippy --workspace --all-targets`: passed on Windows.
- `node --test web/tests/*.test.mjs`: 77 passed (69 web and 8 WASM).
- Production `node web/scripts/build.mjs`: passed, including bundled font assets.
- Worker and deployment tests: 22 passed.
- Offline Chromium production-app checks: Keys/Type/Pointer at 320×568,
  390×844, 844×390, 1024×768 and 1440×900. Dock bounds, pointer-button containment,
  visible language picker, actual shortcut routing, custom Unicode insertion,
  composition deduplication, keyboard viewport reduction, independent FPS
  touches and blur release passed. Compact landscape typing follows the
  reference's 31px character keys; its primary dock remains at least 44px.
- Desktop prototype browser checks: full-width geometry at 1440, 1200 and 800px,
  retained modes and deck placements, compact/full viewport, physical Unicode
  typing, Fit, session settings, focus and saved-FPS geometry passed, with no
  JavaScript runtime exceptions.
- `git diff --check`: passed. The original reference's tracked source has no diff.

The browser test harness now emulates a focused page. Without it, Chromium can
change `activeElement` without a focus event, leaving native input intentionally
cancelled. The former native composition failure reproduced before UI changes;
focus emulation resolves it without changing production input cancellation.

## Desktop correction verification — 2026-11-09

The latest screenshot removes the entire desktop rail. Production CSS selects
hardware-input desktop layout at >=900 CSS pixels, a fine primary pointer and
hover; there is no minimum height or orientation restriction. This deliberately
keeps coarse-pointer tablets and landscape phones on the mobile controls.
All saved deck positions and control modes, including FPS, reserve no rail or
dock space on desktop. A matching media-query change listener releases input
and returns focus to the remote surface when no settings sheet is open.

The production offline browser check passed desktop sizes 1440×900, 1024×768,
1100×500, 1000×1100 and 900×700, across Keys/Type/Pointer and auto/top/bottom/left/
right placements. It asserts full width and height below the 64px header, hidden
touch controls and no FPS dock gap. Actual mouse/key events verify physical
routing, remote focus, blur release and a held-key release when input capability
changes. Settings, encryption label, Fit and disconnect release are checked.
Touch layouts passed 320×568, 390×844, 844×390, 899×700, 1024×768 and 1440×900;
the existing chord, Unicode, native IME, reduced viewport and independent FPS
touch checks still pass. Wide touch landscape uses the existing compact 31px
character keys; primary dock buttons remain at least 44px.

Reran `cargo fmt --all`, `cargo test --workspace`,
`cargo clippy --workspace --all-targets`, all 77 web/WASM tests, all 22 Worker/
deployment tests, the production WASM/web build and both browser scripts: passed.
Inspected production desktop, portrait Type and landscape Keys screenshots plus
the corrected desktop design. The original mobile source has no diff. Audit of
the uncommitted DOM/input/build changes found the validated pin choices, text-only
host name rendering, local font loading and shared session boundaries preserved.
No new protocol, cryptography, transport or CSP changes were made.

## Complete session settings — 2026-11-09

Restored the desktop prototype's Display selector, 25–100% Served size slider,
five Picture quality options, fullscreen preview, Debug / Stats, security and
transport details, and simulated disconnect/reconnect. A host-capability example
switches Display and Served size into explicitly unavailable, disabled states.
Every setting and diagnostic value is labeled as simulated; no host connection
or credentials are involved. Settings now has a visible header label. The sheet
starts at its top, scrolls, keeps Close available, traps focus and returns focus
to its trigger on Escape. The input rail remains absent.

Production settings remain connected to the shared session input path. Added a
visible desktop Settings label, dialog metadata, focus entry/return and Tab /
Escape handling. The Debug / Stats label identifies the existing diagnostics.
Fixed settings-sheet height on short desktop windows to clear the 64px header.

Browser checks passed both unavailable and advertised capability messages on all
six touch layouts and five desktop layouts recorded above. They assert served
size/display/quality packets, selected values, diagnostic content/visibility,
security/fullscreen availability, focus trapping, Escape and header clearance.
Prototype checks passed settings interaction at 1440×900, 390×844 and 844×390,
including unavailable controls, retained selections, diagnostic samples,
disconnect/reconnect, fullscreen preview and focus return. Screenshots of the
settings and Debug sections were inspected on desktop and small viewports.
All 77 web/WASM tests, 22 Worker tests, Rust format/test/clippy checks and the
production build passed again. These are offline browser checks; live-host and
physical-device settings behavior were not revalidated.

## Separate desktop publication

Published using Sites at:
https://topvnc-desktop-wireframe-nov26.example.chatgpt.site

Project: `appgprj_6af1dcf7b3ec819181ff13ee5b7cb32f`.
Latest source commit (complete settings): `b137395da0bd1f087f8554dac79eaaa8604f7268`.
Deployment: `appgdep_6af1ed6bf9f48191a71bec7a52ee5044` (`succeeded`).
Saved version: `appgprj_6af1dcf7b3ec819181ff13ee5b7cb32f~appgver_4b7db66abfd88191b064ee2d4c999249`.
The native Sites result confirmed successful publication at the existing URL.
The existing owner-only audience was verified before publication and preserved.

The desktop Site was created with owner-only access. No source push,
publication, metadata or access mutation targeted the mobile reference Site.
Its tracked source and original project identity are preserved.

## Verification limits

Screenshots and browser interactions use clearly labeled simulated/offline
sessions. No physical phone, Safari, native OS keyboard, real IME, live encrypted
remote session, game compatibility or latency validation is claimed. The
user-facing browser preview host was unavailable; headless local Chromium
provided visual QA. The production Cloudflare app was not deployed.

## Review fixes — 2026-11-11

A code review of `3272255` found regressions in the production viewer. Fixed:

- Closing Settings returned focus to the header button, where hardware keys are
  ignored. Settings and Customize now share one modal stack (`web/modal.mjs`)
  that owns the inert background, Escape, Tab and focus return. Focus returns to
  the opener, normally the remote screen. Escape and Tab work even when focus
  has left the dialog, and no key reaches the host while a dialog is open.
  Closing a dialog no longer un-inerts the page under another open dialog.
- The draft row has a visible **✕** (Clear local draft) again.
- Type and FPS hide the deck heading, so control messages also appear briefly
  over the remote view. The status live region stays in the accessibility tree
  in FPS.
- The pinned **Win** is a one-shot modifier again, restoring Win+key chords.
- If Fn is not pinned, **Fn keys** in the Keys hint row opens F1–F12, Del,
  Home, End, PgUp and PgDn. The Fn toggles now show their open state.
- Landscape Type with the deck at the bottom puts modifiers, input source and
  draft on one row. At 844×390 the remote view is 104px tall instead of 61px.
  The browser check is back to its 89px minimum in every mode. Character keys
  stay at the reference's compact 31px.
- `local_web` serves the two bundled fonts.

Checks: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets`,
gateway clippy with `test-fixtures`, and `cargo test --workspace` passed.
`npm test` passed 75 tests. `local_web` served both fonts as `font/woff2`. The
offline Chromium check passed all touch and desktop sizes with new assertions
for focus return, blocked keys while Settings is open, Escape from body focus,
the visible Clear control, the Type message, Fn states and the footnote toggle.
The production WASM build was not rerun here because the `wasm32-unknown-unknown`
target is not installed in this environment; the WASM and Worker tests were not
rerun.
