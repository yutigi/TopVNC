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
- Users can select, add, remove, and pin language profiles; preferences stay on
  the current device. System keyboard language switching remains explicit.
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
- Primary controls target at least 44 CSS pixels. Keyboard character keys use
  compact phone-like spacing; landscape splits their rows for two-thumb reach.
  Rotation preserves control order and labels.
- Connection/security labels identify simulated state. No real host connections,
  credentials, performance claims, or new platform-support claims are introduced.
- Publish the wireframe privately in Sites and provide its URL for review.
- A visible Customize menu creates, edits, removes, and reorders up to eight
  labeled custom keys. Keys run a demo shortcut or insert a bounded text snippet.
- Users can rearrange language, custom-key, and input groups, reorder the thumb
  dock, choose deck placement, and show/hide optional groups independently for
  portrait and landscape. Layout changes require an explicit Save or reset.
- Saved customization is validated before use, stays local to the browser, and
  preserves reachable Customize and mode controls even when groups are hidden.
- Language pins appear only in Type, never Quick Keys, Pointer, or FPS Play.
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
