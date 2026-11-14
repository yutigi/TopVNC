# Display recovery after a monitor is disconnected

## Scope

Reported: when the primary monitor's HDMI cable was unplugged, the web viewer kept showing the old picture, never updated, and could not be used to control the host. Expected: the host serves a display that is still available with correct input mapping, or, when none can be captured, viewers show a clear unavailable state and recover when a display returns.

This builds on display following ([spec 013](../013-multi-monitor/spec.md)), which handles monitors that Windows removes from the desktop. It covers what spec 013 could not: displays Windows keeps on the desktop after their monitor is gone, a capture started while Windows was still reconfiguring, and no display at all. Authentication, flow control, and the display and served-size controls are unchanged.

## Findings

The failing component could not be reproduced here, since that needs a cable to be pulled. Reading the code against Windows behavior gives two causes that both produce the reported symptom, plus one more race:

1. **Disconnected but still on the desktop.** When a monitor is removed, Windows can keep its display path active while marking the target unavailable (`DISPLAYCONFIG_PATH_TARGET_INFO.targetAvailable`; Microsoft documents this for asynchronous topology changes). GDI and DXGI still list the display as attached, so the layout never changes, and Desktop Duplication waits for frames that never come, without an error. Capture stays on a dead display indefinitely: the picture freezes, and input lands on a desktop nobody can see.
2. **No capturable display.** With the only monitor unplugged, capture fails and is retried, correctly, but viewers are never told. They see a frozen picture that looks like a hung session.
3. **Stale listing at start.** Capture reads the GDI layout and then lists DXGI outputs. If the layout already shows the change but DXGI still lists the old display, capture starts on the removed display. Every later layout reading equals the one taken at start, so no change is ever noticed.

## Acceptance criteria

### Host (Windows)

- Displays without a connected monitor are left out of both the layout comparison and the attached displays. A display counts as connected when at least one active `QueryDisplayConfig` path from its source has an available target. When Windows cannot answer, every display is kept, as before.
- The served display therefore changes, or capture stops, within about a second of the monitor disconnecting, even while Windows keeps the display on the desktop. A remaining display is served when the primary is disconnected, before Windows moves the primary flag. Input maps to the newly served display.
- If the steady layout lacks a display that capture is serving, capture restarts once for that layout. If the two sources keep disagreeing, it does not restart again until the layout changes.
- While capture is stopped, it is retried with the existing backoff (250 ms to 2 s), and resumes on its own when a display returns.
- Capture candidates must match the current GDI monitor name and desktop area, with the primary flag taken from GDI. A stale DXGI output must not override the current main display during reconfiguration.
- An incomplete capture snapshot serves only a confirmed current primary instead of silently renumbering a selected display or representing a partial image as ALL. Refresh that snapshot on each layout check, even when GDI no longer changes, and restore the requested selection once DXGI catches up. A missing secondary or a failed refresh must not interrupt a healthy primary capture. If the current primary is unavailable too, retry capture with the existing backoff.
- If a selected output or ALL cannot start capture while another display is the current primary, try that primary in the same attempt. Keep the requested selection so a later layout change can restore it. Do not retry the same output twice or try other outputs after access to the desktop is denied. Report both failures if the fallback also fails.
- Capture startup succeeds only once each selected output has copied its first desktop image. An error or a one-second timeout before that counts as a failed capture attempt and follows the same primary fallback path. Cursor-only changes and frames from only some selected outputs do not establish a live capture.

### Telling viewers

- New TopVNC private pseudo-encoding `0x54564e43` (TVNC): a payload-free rectangle with `x` = capture status and `y` = width = height = 0. Status codes: 0 live, 1 no display with a connected monitor, 2 secure desktop (lock screen or UAC prompt, which a user process cannot capture), 3 another reason. Viewers treat unknown nonzero codes as 3, so newer hosts can add codes. A rectangle with nonzero `y`, width, or height is a protocol error.
- Viewers advertise the encoding; the host announces only to viewers that did, as soon as the status changes. Viewers start out assuming a live picture. Other viewers still see a still picture.
- A stop shorter than one second, such as a planned restart on another display, is not announced. After that, the last failure's reason is announced, and the live status as soon as capture runs again. The macOS host announces stops too (status 3, as it cannot tell a locked Mac from other failures).
- The browser shows the reason over the last picture, and in the status line, and hides it when the picture is live again or the session closes. The native viewer adds it to the window title.

## Validation (Windows 11, 2026-11-05)

- Formatting, Clippy (`--workspace --all-targets`, no warnings), `cargo test --workspace`, and the web page tests pass on Windows. The macOS host change was not compiled here: the `aarch64-apple-darwin` check needs a C compiler for `ring` that this machine lacks. It must be checked on a Mac before merging, as CI does not run macOS.
- Unit tests:
  - `capture_started_from_a_stale_listing_restarts_once`: a capture planned from a stale listing is restarted once, and not repeatedly when the sources keep disagreeing.
  - `displays_without_a_connected_monitor_are_left_out`: disconnected displays are dropped, the remaining display is served while the dropped one still has the primary flag, the only monitor gone gives no display, and the change restarts capture.
  - `viewers_learn_of_a_capture_outage_only_after_the_grace_period`: covers the grace period, the last reason, and recovery.
  - Protocol parsing, malformed status rectangles, and unknown codes (`topvnc-rfb`).
  - `viewers_are_told_when_the_desktop_cannot_be_captured_and_when_it_returns`: a real server and viewer exchange each status with requested and pushed updates, and nothing else changes on screen.
  - `the_page_learns_when_the_host_cannot_capture_its_desktop`: the browser session raises the event only on change.
- Live, on a two-monitor desktop (`cargo test --bin topvnc -- --ignored gdi_and_dxgi_agree_on_attached_displays every_attached_display_has_a_connected_monitor`): with both monitors connected, every attached DXGI output is reported connected, and GDI and DXGI still agree after filtering.
- Not yet validated: physically unplugging a monitor. Run `cargo test --bin topvnc -- --ignored follows_display_changes_made_by_hand --nocapture` (spec 013) and unplug the primary display's cable. Then repeat with a browser connected and the only monitor unplugged: the notice should appear within about two seconds and clear after the cable is plugged back in.

## Follow-up validation (Windows 11, 2026-11-13)

- Capture startup now checks DXGI candidates against the same GDI snapshot used to start layout following. Only matching device names and desktop areas are accepted, and GDI supplies the primary flag. Incomplete snapshots serve the confirmed primary and are refreshed twice a second until the sources agree, so numbered and ALL selections recover without needing another GDI layout change. A failed refresh keeps a healthy primary running.
- A failed selected-output or ALL capture tries the current primary once, keeping the requested selection for later layout changes. Startup includes waiting up to one second for the first desktop image from every selected output, so first-acquisition errors and timeouts trigger fallback too. Failed attempts release their capture resources before the primary is tried. Secure-desktop failures retain their status and do not trigger another output attempt. Failure of both attempts preserves both errors.
- Thirteen regression tests cover stale names and geometry, a replacement monitor at the same coordinates, incomplete snapshots, fallback to the current primary, restoration without another layout change, refresh failures, the requested monitor returning, ALL fallback, bounded attempts, desktop access denial, first-image readiness and errors, resource cleanup before fallback, and successful capture without fallback.
- Mutation checks: bypassing the readiness check fails `first_frame_failure_and_timeout_follow_the_same_primary_fallback`; disabling refresh fails `a_partial_snapshot_keeps_the_main_live_and_recovers_without_another_layout_change`. Both checks pass with the protections restored.
- `cargo fmt --all --check`, `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and `cargo build --bin topvnc` pass.
- Live Windows checks pass: `gdi_and_dxgi_agree_on_attached_displays`, `every_attached_display_has_a_connected_monitor`, `captures_each_and_all_live_displays`, and `viewers_switch_between_live_displays`. Desktop-capture checks ran outside the sandbox, which denies Desktop Duplication access.
- A physical cable unplug with a browser connected remains unvalidated; use the manual test above. No macOS or Linux hardware testing was performed for this follow-up.
