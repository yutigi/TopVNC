# Send Windows-key shortcuts to the remote computer

## Scope

Reported in [issue #31](https://github.com/yutigi/TopVNC/issues/31): the hotkey **Win+Space**, which switches the input language on Windows, does not work as expected from a TopVNC viewer. The report did not say which client or what happened, so this change fixes the cause that applies to every Windows client: the local Windows acts on Windows-key shortcuts before the viewer sees them.

This covers:

- **Native viewer on Windows:** while the viewer has focus, shortcuts with the Windows key go to the remote computer. The F8 setting **Shortcuts → Win key** turns this off. It is on by default and saved.
- **Web client in fullscreen:** in Chromium-based browsers, Keyboard Lock takes the Windows key while the viewer is in fullscreen. The setting **Settings → Win key in fullscreen** turns this off. It is on by default and saved.

Out of scope:

- Win+L and Ctrl+Alt+Del. Windows handles these before any app can take them, so they always act on this computer.
- Alt+Tab, Alt+Esc, Ctrl+Esc, Alt+Space, and Print Screen. TopVNC still leaves them to this computer, as before.
- The macOS viewer (Cmd+Space, Ctrl+Space) and the Linux viewer.
- The web client outside fullscreen. Browsers do not let a page take these keys there.
- Language-switch hotkeys that do not use the Windows key, such as Alt+Shift, Ctrl+Shift, or the Grave Accent key. Windows still passes these keys to the viewer, so they reach the remote computer, but they also switch this computer's language. That does not change what TopVNC sends, because it sends keys by their US-keyboard position.

## Findings

Read from the code, not reproduced live:

- The native viewer reads keys from winit window events (`src/main.rs`). Windows acts on Win+Space before it posts the Space key to the focused window. So this computer switches its language, and the remote computer receives only Win down and Win up. A Windows key pressed and released alone opens the remote Start menu.
- The Windows host already injects `Super_L` and `Super_R` as `VK_LWIN` and `VK_RWIN` with the extended-key flag (`src/desktop_host.rs`, `src/windows_server.rs`). Nothing on the host side needed to change.
- The web client sends `MetaLeft` and `MetaRight` as `Super_L` and `Super_R` (`crates/topvnc-web/src/keys.rs`), but the browser never sees Win+Space unless Keyboard Lock is active.

## Design

### Native viewer (Windows)

- `src/system_keys.rs` installs a low-level keyboard hook (`WH_KEYBOARD_LL`) for the length of a viewer session. The hook runs on its own thread, which does nothing but run a message loop. Windows waits for low-level hooks on every key pressed anywhere, so the hook never waits behind viewer work such as a network write.
- New presses are taken only while the session is active: **Win key** on, the window focused, and the F8 panel closed. The hook checks the foreground window when the key is pressed. Each activation refreshes the hook registration so it returns to the front of the Windows hook chain.
- A Windows key pressed while capturing starts a chord. Until the Windows keys are released, every new press is taken too. So Windows never sees a shortcut with a Windows key in it, and Space, R, D, and so on reach the remote computer together with the Windows key.
- Repeats and releases follow the original press, even if focus changes or the hook is refreshed. Keys held before the Windows key, such as Shift, stay with Windows.
- Injected keys always pass through. Otherwise a TopVNC server on the same computer, which injects the keys its viewers send, would loop them back.
- Taken keys are named from their scan code the same way winit names window keys. They reach the event loop as `UiEvent::SystemKey` through the same queue as other wake-ups, so a chord arrives in the order it was typed. The session sends them like other keys, with the same pressed-key tracking. So a focus loss or opening F8 still releases them on the remote computer.
- When capture is enabled and a Windows key reaches the regular window input handler, Windows may have kept the rest of the shortcut. The viewer leaves that press local to avoid opening Start on the host. A release reaching the window requests a hook refresh for the next attempt. The hook itself performs no debug logging because a blocked callback can cause Windows to remove it.
- If the hook cannot be installed, the viewer prints `Windows-key shortcuts will act on this computer: …` and works as before.

### Web client

- `web/system-keys.mjs` calls `navigator.keyboard.lock(["MetaLeft", "MetaRight"])` when the viewer enters fullscreen with the setting on, and `unlock()` otherwise. Browsers without Keyboard Lock, and refused locks, change nothing.
- Only the Windows keys are locked. Escape still leaves fullscreen as before.

## Acceptance criteria

- [x] In the Windows native viewer with **Win key** on and focus, Win+Space reaches the remote computer as Win down, Space down, Space up, Win up, and this computer's language does not change.
- [x] Every key pressed while a taken Windows key is held goes to the remote computer, in order.
- [x] Keys that were down before the Windows key, and all keys while unfocused, F8 open, or **Win key** off, act on this computer as before.
- [x] No key stays down on this computer or on the remote computer after a shortcut, a focus change, or the end of the session.
- [x] Injected keys pass through the hook.
- [x] The F8 switch and the web setting are saved and default to on.
- [x] In Chromium fullscreen with the setting on, the web client locks only `MetaLeft` and `MetaRight`. Otherwise it unlocks them.
- [ ] With the updated Windows viewer, Win+Space switches the host's input language on the first attempt after connecting and after refocusing the viewer. If Windows takes a chord and delivers the Windows key release to the viewer, the next attempt works after the hook refresh and the host Start menu stays closed.

## Validation (Windows 11, 2026-11-27)

- Unit tests (`cargo test -p topvnc --bin topvnc -- system_keys`):
  - `a_windows_key_shortcut_is_taken_whole_and_released_whole`, `keys_pressed_before_the_windows_key_stay_with_windows`, `nothing_new_is_taken_without_focus_but_taken_keys_still_come_back`: the chord rules.
  - `a_windows_key_held_when_capture_starts_stays_local_until_released`: local presses stay local when capture activates mid-press.
  - `the_hook_sends_win_space_to_the_viewer_in_order` and `the_hook_leaves_injected_keys_and_other_windows_alone`: the hook's decisions, the keys it sends, and their order.
  - `hook_keys_are_named_like_window_keys`: scan codes map to the same winit keys as window events.
  - `the_hook_installs_and_stops_with_its_capture` (ignored; run with `--include-ignored` on a desktop): the real hook installs, accepts a refresh request, and stops with its thread when dropped.
  - `a_refresh_restores_a_hook_that_windows_removed` (ignored by default): removes the real Windows hook, then verifies that refreshing installs a usable replacement.
  - `session_overlay_reflects_windows_key_changes`: the F8 panel redraws when the switch changes.
- Web unit tests (`npm test` in `web/`): `tests/system-keys.test.mjs` covers locking, unlocking, browsers without the API, and refused locks.
- PR #32 live test on 2026-11-27, Windows 11 25H2 viewer to a Windows 11 TopVNC host (English and Thai): Win+Space reached the host as `system key` events in order. In the first minute or two of fresh sessions, Windows sometimes skipped the hook while the viewer was in front and handled the shortcut locally. The hook still received keys typed in other windows and injected test keys. The cause remains unconfirmed. Run the updated viewer with `--input-debug` to see hook installation, capture changes, and refresh requests when Windows delivers a Windows key to the viewer window.
- Remaining live steps, which need a person at the keyboard because the hook ignores injected keys:
  1. On a Windows viewer computer with two input languages, connect to a Windows host that also has two. Click into the viewer and press Win+Space. The host's language switches, and this computer's does not.
  2. Press and release Win alone. The host's Start menu opens, and this computer's does not.
  3. Hold Shift, press Win+S, and release. The host gets Shift+Win+S, and Shift is not stuck on either computer.
  4. Hold Win, click another window on this computer, then release Win. Neither computer has Win stuck.
  5. Open F8 and switch **Win key** off. Win+Space switches this computer's language again. Reopen the viewer and check that the switch stayed off.
  6. Press Win+L. This computer locks, as expected.
  7. In Chrome or Edge, open the web client, enter fullscreen, and press Win+Space. The host's language switches. Leave fullscreen and press it again: this computer's language switches.
