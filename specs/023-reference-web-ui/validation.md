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
https://topvnc-desktop-wireframe-nov26.js-sookudom-temp.chatgpt.site

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

### Mobile reset-to-fit follow-up

Added **Settings → Fit to screen** alongside fullscreen, using the toolbar's
existing view reset. It clears local zoom/pan, closes Settings and returns focus
to the opener. It remains reachable when phone layouts hide the toolbar Fit.
The offline Chromium check zooms the canvas, clicks the settings action by
screen coordinates, and verifies the fitted transform, focus return, and no
host command beyond the input release on opening Settings. It passed at all
six touch and five desktop sizes above, including 320×568 and 390×844 phones.
All 75 web unit tests, `cargo fmt --all -- --check`, `cargo test --workspace`,
and `cargo clippy --workspace --all-targets` passed. Browser checks used local
Chromium with GPU and sandbox disabled for the offline fixture. The production
WASM build remains unverified because its Rust target is not installed.

### Reference fidelity and golden tests — 2026-11-11

The earlier viewport screenshots were not reference goldens. A new comparison
renders the unchanged mobile wireframe source and the actual web UI, with the
same locally bundled fonts. The hosted Site was identified through its Sites
metadata, but its private live page could not be opened through the browser
host. No source, publication or access changes target that Site.

Corrected inherited session font metrics (16px/normal rather than 15px/1.5),
reference button padding and gaps, the tinted Customize action, 44px arrow
columns including the reference's late overrides, portrait hint spacing, Type
modifier/source spacing, compose text size, keyboard bottom-row widths, split
keyboard offsets and dock/trackpad colors. Host names and Insert use real
weight 600. DM Sans's bundled variable font is byte-identical to the reference's
official Google Fonts file; it did not need replacing. Preserve native text
entry at 16px and the readonly local draft at the reference's 13px.

`npm run test:visual` now compares 24 reference-derived component PNGs, exact
computed typography/color/padding and <=1px geometry at 390×844 and 844×390.
Portrait Keys/Type also compare deck offsets. Reference-only baseline generation,
canonical source/font hashes, the renderer version, masks and intentional
accessibility/layout adaptations are documented in
`web/tests/golden/mobile-reference/README.md`. A Windows CI job runs the
behavioral browser check and these goldens. Full viewport comparisons, measured
differences and component diff PNGs are emitted to `web/build/reference-qa/`.

All 24 goldens passed: 21 were pixel-identical, with the remaining three changing
at most 1.2% of pixels within the documented small-text raster tolerance.
A deliberate test-only weight-700 regression produced
34 rejected style/pixel checks; a subsequent unmodified run passed again.
The 75 web unit tests, all six touch and five desktop browser sizes, production
WASM build, and eight WASM binding tests passed. With the repository's pinned
Rust 1.99.0, `cargo fmt --all -- --check`, `cargo test --workspace` and
`cargo clippy --workspace --all-targets` passed (the existing timing benchmark
remains ignored). The inherited shell override selected Rust 1.95 initially;
using 1.99.0 resolved the misleading missing-WASM-target build error.

Visual QA uses offline session fixtures in local Windows Chromium. CI has been
configured but not run remotely here. Physical-device/Safari/native-IME and
live-host verification remain outside these results. The production client
and reference Sites were not deployed.

### Pointer, Type and placement follow-up — 2026-11-11

Removed the Pointer footer containing `Screen input: Trackpad` and the mode
toggle. Session settings now exposes the existing saved landscape placement
(default, left, right, bottom) without changing portrait preferences. Both side
layouts have the divider next to the remote view. Browser checks exercise
Keys/Pointer/Type on both sides, reload persistence and rotation to/from portrait.

Type now matches the accepted composition row: no extra Clear button, the
reference Insert width/wrapping/border and regular weight 400. This supersedes
the earlier statement that Insert uses 600. Landscape modifiers, input source
and draft occupy separate full-width rows; compose height/padding and spacing
match the reference. Short viewports scroll the keyboard above a fixed dock.
English help copy matches the reference; native/Chinese/Japanese guidance stays
specific to the implemented input path.

Extended the unchanged reference's goldens from 24 to 30 to cover the draft,
compose field and Insert, and removed the landscape width comparison exception.
Landscape Type row offsets are now checked too. All 30 visual comparisons,
76 web unit tests and the behavioral browser suite passed in local Windows
Chromium with GPU and sandbox disabled for the offline fixture. Workspace
format, tests, Clippy and the production WASM/web build passed with Rust 1.99.0. The reference source was not
modified. No hosted application was deployed by this follow-up.

### Requested mobile Site publication — 2026-11-11

The user subsequently requested both the local comparison page and the existing
mobile Site be updated. The regenerated comparison retains the 30 passing
reference checks and now includes Settings and both landscape sides for
Keys/Type/Pointer. The generator writes the page's refresh timestamp.

Opened the mobile Site's current remote source in an isolated checkout under
`web/build/mobile-site-source/.sites-checkout`. Added the same landscape setting
to its Session settings, using its existing customization preference, and fixed
the left-side divider. Its Pointer and Type controls already use the accepted
layout. All 27 prototype tests and offline browser checks of both sides, modes,
reload persistence and portrait isolation passed. The accepted reference
snapshot in `design/topvnc-mobile-wireframe` remains unchanged for goldens.

Published the mobile Site at its existing URL with owner-only access preserved:
https://topvnc-mobile-wireframe-nov26.js-sookudom-temp.chatgpt.site/

Source commit: `dd7dec62d7cba9c31392f1f3c8ed3897f3e075b9`.
Deployment: `appgdep_6af36439a67c8191a07b26fa215b12cd` (`succeeded`).
Saved version: `appgprj_6aeedaa6ea0c81918528d7a80229d4d3~appgver_a84960d192ac81919b679402553c5d55`.
The native Sites result confirmed the production URL. This publishes the design
prototype only; the production Cloudflare VNC client was not deployed.

### Landscape trackpad fill — 2026-11-11

The side deck previously left 116px unused below its fixed-height trackpad and
click buttons at 844×390. The Pointer panel and trackpad now grow within the
available deck, with both 44px click buttons directly below the pad and above
the dock. Portrait and FPS geometry stay unchanged. A browser assertion failed
on the original unused space and passes with the correction, including both
saved sides. The comparison page and placement screenshots were regenerated.

The accepted reference snapshot remains unchanged: the landscape trackpad's
styling is checked at its original height, while behavioral checks verify its
actual fill and button/dock bounds. All 30 visual comparisons, 76 web tests,
the behavioral browser suite, production build, Rust format/workspace tests/
Clippy, and the prototype's 27 tests passed. Offline prototype browser checks
also verify trackpad expansion and button placement on both sides.

Published the correction to the existing owner-only mobile Site. Source commit:
`6941b3f2e5b88703152d17bda5196547f032fff6`. Deployment:
`appgdep_6af36781457c81918be3516c80a5fc87` (`succeeded`). Saved version:
`appgprj_6aeedaa6ea0c81918528d7a80229d4d3~appgver_8f70ebe44ebc8191a07d51fe99195273`.
The native Sites result confirmed the existing production URL and the audience
was preserved. The full-width landscape Type layout is selected with Session
settings → Landscape controls → Bottom (or Default), then Type in the dock.

### Review follow-up — 2026-11-11

The local preview commit accidentally changed the accepted reference's
`customize.css`, causing `test:visual` to fail its provenance assertion. Restored
that file to the recorded reference snapshot. No baseline hashes or PNGs were
regenerated; all 30 reference comparisons now pass.

Landscape Type now keeps its Edit layout action in the scrollable control panel
with a 44px target. Short viewports can scroll to it above the fixed dock. The
new browser regression failed at 844×390 before the CSS correction. It now
checks actual pointer clicks, language-dialog opening, Escape dismissal and
focus return across the existing portrait/landscape sizes and both side decks.

Validation on Windows: 76 web tests, the behavioral browser suite and 30 visual
comparisons passed. `cargo fmt --all`, `cargo test --workspace --locked`
(355 passed, 8 ignored), Clippy for all workspace targets with warnings denied,
and `git diff --check` passed. Browser evidence uses the offline session fixture;
physical-device, Safari and live-host verification remain outside these checks.
