# Compact session title and debug toggle

## Scope

Shorten the viewer title and make detailed performance diagnostics optional
through a button in the existing F8 settings panel.

## Acceptance criteria

- Debug is off on launch. The normal session title shows TopVNC, the desktop
  name capped at 24 characters plus an ellipsis, and rendered FPS.
- Preserve a compact `locked (F8)` hint whenever the pointer is locked.
- F8 offers a visibly selected `DEBUG: ON` / `DEBUG: OFF` button. Switching it
  immediately shows or hides the latest sampled diagnostics without reconnecting.
- Debug shows the existing decode/render rates, bandwidth, encoding, push state,
  presentation timings/mode, superseded snapshots, and Ultra indicator.
- Debug affects display only; input logging stays an explicit CLI option.
- Keep the security warning visible and all controls within the existing panel.

## Validation

On Windows, formatting, workspace Clippy, and the release build pass. Inspected
renderer captures with debug off/on: both buttons fit and the transport warning
remains visible. Updated the README's panel captures.

The workspace test run reports a timeout in the unchanged library test
`clients_without_desktop_size_are_disconnected_on_resize`, also reproduced alone.
With that test filtered out, 159 tests pass and three remain intentionally ignored.
Live GUI interaction and macOS/Linux builds were not validated here.
