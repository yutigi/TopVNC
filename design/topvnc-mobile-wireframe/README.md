# TopVNC mobile control wireframe

Interactive design prototype only. No live hosts, passwords, or remote input.

## Review in Sites

Open the [published wireframe](https://topvnc-mobile-wireframe-nov26.js-sookudom-temp.chatgpt.site).
Select Portrait or Landscape; use **Try full
viewport** on a phone to try the layout at its actual viewport size.

## Review flows

- **Type:** the custom TopVNC keyboard is the default. Tap character keys, use
  Shift, symbols, Space, or hold Backspace; Insert puts the phrase in the demo
  remote editor. Four thumb-dock controls keep their order across orientations.
- **Languages:** EN, Thai, Chinese, and Japanese are editable examples. The +
  panel adds/removes language profiles and pins up to four. Edit each profile's
  three character rows. Preferences are saved only in this browser.
- **Chinese/Japanese:** type `nihao` or `zhongwen` in Chinese, or `nihon` or
  `arigatou` in Japanese, then select a candidate and Insert. Candidates are
  explicitly labeled demo fixtures; no complete conversion engine is included.
- **Native input:** switches to a real editable field labeled with the detected
  OS. Your browser and OS control whether an on-screen keyboard opens. The
  website does not switch OS languages; use the native keyboard's globe key.
- **Keys:** tap modifiers for the next key, configure four pinned shortcuts, or
  open the Fn grid. Mode/rotation/focus changes release modifiers.
- **Pointer:** drag in the demo trackpad to move the sample cursor.
- **FPS preset:** select FPS preset in the sidebar or Customize → Presets → Use
  FPS preset. The Play dock button opens thumb controls over a simulated training
  scene: WASD, sprint, crouch, fire, aim, jump, reload, use and weapon. Hold multiple
  buttons with separate fingers and drag the look zone to move the demo crosshair.
  The same controls work in portrait, with a larger view in landscape.
- **FPS configuration:** remap each game button, mirror the aiming hand, and set
  simulated look sensitivity in Presets. Standard layouts and custom keys stay
  saved. Switch to Type for chat; the Play button returns to the FPS overlay.
- **Language pins:** appear only in Type, never in Quick Keys or FPS Play.
- **Customize → Your keys:** create up to eight labeled single-key shortcuts,
  modifier chords, or text snippets. Edit, remove, move up/down, and choose a
  two-column button. Changes save in this browser and appear in the custom pad.
- **Customize → UI layout:** independently configure portrait and landscape deck
  placement, control-group order, mode-dock order, optional language/custom-key
  visibility, and standard/large custom-key sizing. Save applies the arrangement;
  Reset selects defaults for the chosen orientation. All modes and the header's
  Customize entry stay reachable. Longer custom decks scroll above a sticky dock.

In landscape, Type uses a wide split keyboard below the remote display. Keys
and Pointer use a side deck. Character keys use compact keyboard spacing; the
primary dock controls remain at least 44 CSS pixels high.

## Verification limits

JavaScript syntax, CSS parsing, HTML element references, and the customization
model tests are checked. The initial version also passed local HTTP checks;
subsequent review uses Sites without starting a local server.
Browser automation was unavailable: the browser host failed, and the Windows
fallback stopped because it could not verify the active URL. Visual rendering,
real-device thumb reach, native keyboard visibility, and IME behavior need
manual validation. This prototype makes no latency or platform-support claim.

Source lives here. `.sites-runtime/` is an ignored, separate Sites publication
checkout and package area; it is not part of the application's Git history.
