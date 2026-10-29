# Multiple host displays

## Scope

A Windows host must follow display changes: when a monitor is unplugged, plugged in, or another monitor becomes primary, viewers should see the display they asked for, not a frozen image of a removed one. Hosts with several monitors need a visible way to choose one, and a way to serve all of them at once. Viewers can switch the host's display during a session. The choice is shared by all viewers, like the served size. Authentication, flow control, and the served-size control are unchanged.

## Acceptance criteria

### Display selection

- `--display primary|NUMBER|all` selects the served display. `primary` (the default) follows the primary display when another one becomes primary. `NUMBER` (1 to 16, in DXGI enumeration order on Windows) serves that display, and while it is not attached, the primary display is served with a notice; the numbered display is served again once it returns. `all` serves every attached display in one framebuffer arranged as on the desktop. Areas no display covers are black, and mirrored displays are captured once.
- If all displays together exceed the 8192-pixel or pixel-count framebuffer limits, the primary display is served with a notice instead of a truncated or frozen image.
- The Server tab replaces the free-text display number with a picker. Each click moves to the next choice: main display, each detected display, then ALL. The button shows the choice and its pixel size, for example `2: 2160X3840` or `ALL 5600X3840`. A display that is not attached shows `NOT FOUND`. The list is refreshed every second while the tab is shown and the server is stopped.

### Following display changes (Windows)

- Twice a second the host compares the GDI monitor layout (device names, desktop areas, primary flag), which costs microseconds. A changed layout is acted on once it reads the same twice in a row, so intermediate states during reconfiguration are ignored.
- On a settled change, the selection is resolved again against DXGI outputs. Capture restarts only if it no longer covers exactly the displays to serve; otherwise only the announced display numbers and count are updated.
- Desktop Duplication errors (access lost on mode changes or unplugged outputs) still restart capture with backoff, which now resolves the selection again.

### Capturing several displays (Windows)

- Each captured display has its own Direct3D device and Desktop Duplication on its own thread, so a display without changes never delays frames from another. Threads copy dirty and moved regions into one shared image at the display's offset and wake the serving loop. Pending regions are bounded (256) and collapse to the whole image beyond that.
- The cursor follows the display that last reported it visible; a display that does not show the pointer cannot hide it. The pointer position is placed by that display's offset. Pointer input maps across the whole served area of the virtual desktop.
- A process can duplicate each output only once, so the old capture is always stopped before a new one starts.

### Viewer control

- TopVNC private pseudo-encoding `0x54564e44` (TVND). The host announces a payload-free rectangle with `x` = served display (0 = all, otherwise 1..count), `y` = attached count (1..16), and width = height = 0. Viewers validate the announcement and reject anything else.
- Requests are standard Fence requests whose payload is ASCII `TopVNC.Display/1:` followed by one byte: 0 for all displays or a display number up to 16. A number above the attached count is accepted, because the viewer may not know about an unplug yet; the host then serves the primary with a notice. Requests are rejected unless the client advertised both the capability and DesktopSize and the host offers switching.
- F8 shows a DISPLAY row with one button per display (up to eight) and ALL. The served display is highlighted, and a click sends one request. FIT, 1:1 PIXELS, and FULL SCREEN now share one row so the panel still fits a 720-pixel window. Hosts that do not offer the capability show `NOT OFFERED BY THIS HOST`.

### macOS

- macOS hosts keep their existing behavior: the main display is followed, and numbered displays use ScreenCaptureKit order. `--display all` is refused with a clear error, and the Server tab does not offer ALL there. macOS hosts do not offer viewer display switching.

## One port per display

Serving each display on its own port is not built into one server process. Separate processes work today: `topvnc --serve 0.0.0.0:5900 --display 1` and `topvnc --serve 0.0.0.0:5901 --display 2`. This was checked on Windows by starting both on loopback; each listened and served its own display. Viewers then open one window per display, and each stream has its own flow control. The `all` mode instead sends one stream, whose unchanged regions cost no bandwidth with Tight; lower the served size to reduce it further.

## Validation (Windows 11, 2026-10-29)

- Host: two monitors, 3440×1440 primary at 100% scale and 2160×3840 portrait at 150% scale. Both GDI and DXGI report physical pixels in the capture threads' per-monitor-aware context; the Server tab reads them the same way.
- Formatting, Clippy with warnings denied (Windows, and `aarch64-apple-darwin` type-checking), and the workspace tests pass. The documented `clients_without_desktop_size_are_disconnected_on_resize` test still fails on Windows, identically on the unchanged `main` code, and was excluded.
- Live tests, run with `cargo test --bin topvnc -- --ignored --test-threads=1` on that desktop:
  - GDI's monitor layout matches DXGI's attached outputs.
  - Each display alone, the primary, and all displays capture at the expected size, and every display's area of the combined 5600×3840 image receives pixels. Re-selecting the captured display by number keeps the capture; another display or ALL requires a new one.
  - A viewer connected over loopback switches from all displays to display 1 (3440×1440), display 2 (2160×3840), and back, through the protocol, with matching DesktopSize changes and announcements.
- Unit tests cover selection parsing, fallback to the primary, oversized and mirrored layouts, settled layout changes, cursor ownership across displays, offset copying with rotation and clipping, protocol negotiation, malformed announcements, and request validation.

### Proof of following display changes

The decision of what to serve after a viewer request or a layout reading lives in the platform-neutral `DisplayFollower` (`src/desktop_host.rs`), which the Windows serving loop calls. Its tests feed it the layouts Windows reports, modeled on the test machine, in which the new primary moves to the desktop origin:

| Test | Scenario |
| --- | --- |
| `primary_selection_follows_a_new_main_display` | The main display changes to display 2 and back; capture restarts on the new main display each time. |
| `unplugging_the_served_primary_moves_to_the_remaining_display` | The served main display is unplugged; the remaining display is served. |
| `unplugged_chosen_display_falls_back_to_the_primary_until_it_returns` | Display 2 is chosen and unplugged: the primary is served with a notice, and display 2 again once it is plugged back in. |
| `changes_to_other_displays_keep_capture_and_only_renumber` | Unplugging or adding another display keeps capture running and only renumbers. |
| `all_displays_follow_monitors_added_and_removed` | ALL recomputes its area as monitors come and go. |
| `brief_layout_glitches_never_restart_capture` | States that last one reading during reconfiguration are ignored. |
| `viewer_requests_restart_capture_only_for_other_displays` | Requests restart capture only when other displays are needed; stale numbers fall back. |
| `viewers_follow_simulated_monitor_changes` | End to end with a real RFB server and viewer and solid-color simulated displays. The viewer's size, pixels, and announced display follow a main-display change, requests for ALL and display 2, display 2 being unplugged, and display 2 returning; input still arrives afterward. A 30-second watchdog fails instead of hanging. |

Mutation check: with layout following disabled, as the code behaved before this feature, the first five tests and the end-to-end test fail; with the fix, all pass.

The remaining links run on real hardware: `gdi_and_dxgi_agree_on_attached_displays` shows that the layout readings come from the same displays DXGI captures. `viewers_switch_between_live_displays` exercises the same stop-then-restart path a display change uses, on live capture.

The physical change itself needs a person. `follows_display_changes_made_by_hand` serves the primary display to a viewer on loopback for up to two minutes (`TOPVNC_MANUAL_SECONDS`). Meanwhile, change the main display or unplug a monitor. The test passes when the viewer shows the new primary's size and number:

```sh
cargo test --bin topvnc -- --ignored follows_display_changes_made_by_hand --nocapture
```

Its harness was checked with no change made: it served and showed display 1, then failed after five seconds with "no display change was made". It has not yet passed with a real change.

- Not yet validated: a physical unplug or main-display change on live hardware (the test above), cursor drawing across displays, and capture latency with a game in ALL mode. No performance improvement is claimed.
