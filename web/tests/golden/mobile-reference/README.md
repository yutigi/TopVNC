# Reference-derived mobile goldens

These PNGs are rendered from the unchanged source in
`design/topvnc-mobile-wireframe/dist`, the source of the accepted
[mobile wireframe](https://topvnc-mobile-wireframe-nov26.example.chatgpt.site/).
They are **not** screenshots approved from the production implementation.
The original screenshot check only verified bounds and behavior, so it could
not catch typography, color or spacing drift.

Run with Node 22 and an installed Chromium browser:

```powershell
$env:TOPVNC_BROWSER='C:/Program Files/Google/Chrome/Application/chrome.exe'
npm run test:visual
```

CI runs this on Windows with the behavior checks. The manifest records the
baseline Chromium version, canonical LF source hashes and binary font hashes.
Fonts are embedded locally, and the check rejects external HTTP resources.
DM Sans is the same variable font served by the reference's Google Fonts URL:
`https://fonts.gstatic.com/s/dmsans/v17/rP2Yp2ywxg089UriI5-g4vlH9VoD8Cmcqbu0-K4.woff2`.
Its SHA-256 is `9fea608a947e67020c33cad9a6fe3d60c54119dfb8cff87768a8117a15ed7543`.
It contains a real weight axis; the session heading uses 600. The reference's
Insert action and regular control labels use 400.

The 30 component snapshots cover Customize, pins, modifier/arrow groups,
keyboard source, draft row, compose field, Insert, character rows, keyboard bottom row, trackpad, mouse buttons
and dock across Keys/Type/Pointer at 390×844 and 844×390. Exact computed
typography/color/padding checks accompany <=1px geometry comparisons. Portrait
Keys/Type and the landscape Type rows also compare offsets within the deck, catching spacing regressions
that isolated control snapshots would miss. Pixel comparisons allow a channel
delta of 16 and at most 2% changed pixels for small-text raster differences.
Windows Chromium was verified here; other OS renderers can differ.

The comparison excludes prototype workbench chrome, bezel/annotations,
simulated framebuffer and production authentication/security copy. Intentional
production adaptations are explicit in the harness:

- Landscape pins and mouse buttons keep a 44px touch-target floor.
- Landscape Type preserves full-width modifiers, source switch and draft rows.
  Short viewports allow scrolling the keyboard above the fixed dock.
- The trackpad's different explanatory `<small>` text is masked on both sides.
- The landscape trackpad grows to fill the available deck above both click
  buttons. Its original styling is compared at the reference's compact height;
  browser checks separately assert the real fill and button/dock bounds.
- Components are captured on their white deck background at an integer origin,
  avoiding scroll clipping and the prototype's sticky dock overlapping a key.
- Live feedback, host capabilities and desktop hardware input
  remain covered by the behavioral browser checks rather than prototype pixels.

Inspect `web/build/reference-qa/comparison.html` for paired full viewport shots,
`report.json` for measurements and `diff-*.png` for component differences.
These are ignored build artifacts. Sessions are offline fixtures, not live
remote desktops or evidence of physical-device/Safari support.

To prove font regression detection without changing source or saved goldens:

```powershell
node scripts/check-reference-ui.mjs --probe-font-regression
```

It injects weight 700 only into the test page and succeeds only when both style
and pixel comparisons reject that change. Rerun the normal check afterwards
to restore the generated review artifacts.

When an intentional reference revision requires new baselines:

```powershell
node scripts/check-reference-ui.mjs --update-reference
```

This always renders **reference** source for baseline PNGs, then checks the
implementation. Review source/font provenance, component PNGs and viewport
comparisons together. Do not replace a failing golden with an actual-client
screenshot. Never change or publish the reference Site to make this test pass.
