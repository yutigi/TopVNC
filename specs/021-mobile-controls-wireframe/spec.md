# Mobile web control wireframe

Date: 2026-11-07. Status: design prototype only, before production implementation.

## Scope

Create a separate Sites wireframe for the TopVNC web viewer. Prioritize familiar,
stable mobile key locations in portrait and landscape and configurable language
access. English, Thai, Chinese, and Japanese are examples, not a fixed language list.
Keep the working web viewer and transport code unchanged.

## Acceptance criteria

- An interactive prototype shows portrait and landscape remote-viewer layouts.
- Keyboard, pointer, and quick-key modes have fixed, labeled controls.
- Escape, Tab, modifier keys, navigation, and user-pinned shortcuts are easy to
  reach without horizontally scrolling the primary controls.
- Users can select, add, remove, and favorite language profiles through the icon
  inside the custom keyboard; preferences stay on the current device. Previously
  saved pins become picker favorites. System language switching remains explicit.
- The globe key cycles languages on tap. Holding opens an anchored menu; sliding
  highlights a language and releasing selects it. Releasing over the globe keeps
  the menu open for tapping/scrolling; releasing outside cancels. Current language
  and hover state are distinct. The menu includes a separate management action,
  keyboard/assistive access, and dismissal on Escape, outside press, focus loss,
  rotation, mode/source changes and pointer cancellation. Composition is preserved.
- Type opens a custom TopVNC on-screen keyboard by default. Language profiles
  include editable character rows; English and Thai example layouts type locally.
- Chinese Pinyin and Japanese romaji demonstrate explicit candidate selection
  using small labeled fixtures, without claiming a complete custom IME.
- An optional native-input switch identifies the OS and focuses a real editable
  field. The OS/browser controls whether its native on-screen keyboard appears.
- Typing provides a local composition area and an explicit insert action; input
  is not sent while a native IME is composing. The prototype only changes demo text.
- Modifier state is visible, reset after a chord, and released on mode changes,
  orientation changes, cancellation, or focus loss.
- Type replaces the separate pinned-language row with Ctrl, Alt and Shift.
  Shift shares state with the keyboard Shift and updates character labels.
  Shifted characters insert locally; Ctrl/Alt character and control-key chords
  produce simulated key feedback without inserting text or the composed phrase.
  Native input uses OS modifiers; its Type modifier buttons are disabled. Native
  IME composition guards prevent shortcuts, profile changes and insertion.
- Primary controls target at least 44 CSS pixels. Keyboard character keys use
  compact phone-like spacing; landscape splits their rows for two-thumb reach.
  Rotation preserves control order and labels.
- Connection/security labels identify simulated state. No real host connections,
  credentials, performance claims, or new platform-support claims are introduced.
- Publish the wireframe privately in Sites and provide its URL for review.
- A visible Customize menu creates, edits, removes, and reorders up to eight
  labeled custom keys. Keys run a demo shortcut or insert a bounded text snippet.
- Users can rearrange Type-modifier, custom-key, and input groups, reorder the thumb
  dock, choose deck placement, and show/hide optional groups independently for
  portrait and landscape. Layout changes require an explicit Save or reset.
- Saved customization is validated before use, stays local to the browser, and
  preserves reachable Customize and mode controls even when groups are hidden.
- The replacement modifier row appears only in Type, never Quick Keys, Pointer,
  or FPS Play. Saved language-row order migrates to modifier-row order; the old
  hide-language preference cannot hide modifiers. No language chips remain above Type.
- An FPS preset provides WASD, sprint/crouch, fire/aim, jump, reload, use, and
  weapon controls over a clearly simulated training view. It supports independent
  simultaneous touch holds and a drag-to-look zone in both orientations.
- FPS bindings, aiming hand, and simulated look sensitivity are configurable.
  The preset preserves standard layouts, language preferences, and custom keys.
- Mode/orientation changes, focus loss, pointer cancellation, and hidden-page
  events release simulated held input. No real FPS/game support is claimed.

## Validation

Check JavaScript syntax and inspect the wireframe in a browser at portrait and
landscape sizes when preview tooling is available. Physical-device thumb reach,
native keyboard/IME interaction, and remote input latency need later testing.
