# Mobile controls in the web client

Date: 2026-11-07. Status: implemented; automated checks passed. Device verification limits are recorded in validation.md.

## Scope

Integrate the accepted feature 021 mobile controls into the real `web/` viewer.
Keep pairing, encrypted session establishment, reconnect, host display/size,
quality, fit/zoom, fullscreen, physical input and native Unicode composition.
Keep protocol and transport logic outside the UI. No hosting deployment, native
client changes, custom conversion engine, performance or platform support claim.

## Acceptance criteria

- A stable Keys / Type / Pointer / Enter dock works in portrait and landscape.
  Primary controls have 44 CSS pixel targets and decks leave remote viewing space.
- Type defaults to a custom keyboard with a local draft and explicit Insert.
  Ctrl/Alt/Shift apply once to the next key/chord; Shift labels match output.
  Control chords never insert the draft. Native input retains the existing
  beforeinput/input/composition deduplication, with custom modifiers disabled.
- The keyboard globe cycles editable language profiles on tap. A 350 ms hold,
  slide and release selects from an anchored, accessible menu; releasing on the
  globe leaves it open and releasing outside cancels. Keyboard navigation,
  cancellation, mode/source/orientation changes, hidden page and blur dismiss it.
- English, Thai, Chinese and Japanese are editable examples, with at most 16
  profiles, four favorites and bounded three-row layouts. Chinese/Japanese use
  direct characters or native IME; no simulated candidate fixtures ship.
- Users can save at most eight custom chords/snippets and independent portrait
  and landscape deck placements, group/dock order and custom key sizes. Validate
  all stored data and migrate legacy language-row ordering without hiding the
  required Type modifiers. Storage failure leaves usable session preferences.
- FPS Play supports independent simultaneous movement/action holds and aim drag,
  configurable bindings, sensitivity and handedness. Output uses real session
  APIs, deduplicates shared bindings, and releases on every cancellation/exit.
- Mode/source/orientation changes, disconnect, blur, hidden page, page exit and
  cancelled/lost pointer capture release held inputs without stale repeats.
- Focused Node integration/model tests, web build/WASM checks and the repository
  Cargo format/test/Clippy gates pass. Document browser/device verification limits.
- Review and commit completed work in this worktree. Do not push or deploy.

## Validation scenarios

Assert modifier down/key down/key up/modifier up order through `createInput`;
composition emits committed Unicode once; custom text cannot overflow bounds;
language gestures select/cancel correctly; two fingers sharing a binding retain
it until both release; source/mode/rotation/blur/disconnect clear local and remote
holds. Inspect markup references and bundle assets. Real-device IME, touch reach,
rendering and latency remain manual verification requirements.
