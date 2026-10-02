# macOS server host

## Scope

Add a macOS host backend so the Server tab and `topvnc --serve` can share a Mac display with the same RFB server, security rules, and UI as the Windows host (spec 004). The backend captures one display with ScreenCaptureKit, injects remote keyboard and mouse input with Quartz events, and syncs clipboard text. Protocol code in `src/lib.rs` is unchanged. Platform-neutral host logic stays in `src/desktop_host.rs`, and macOS API calls live in a new `src/macos_server.rs` compiled only for `target_os = "macos"`.

Out of scope: Apple Remote Desktop authentication, capturing or unlocking the login window, curtain mode, audio, running as a LaunchDaemon or before login, preventing sleep, and serving more than one display per process.

## Acceptance criteria

### Shared host logic (`src/desktop_host.rs`)

- Key ownership tracking (`RemoteInputState`) is shared by both backends. It no longer depends on Windows virtual-key codes, so each backend supplies its own key type, and per-client release behaves the same on both platforms.
- A keysym → macOS virtual key code table (ANSI layout, `kVK_*` values) covers printable ASCII, editing and navigation keys, the function keys F1–F20, the keypad, and modifiers. Left and right modifiers stay distinct. Alt maps to Option, Super and Meta map to Command, and Control maps to Control. Volume up, volume down, and mute map to their key codes. Other keysyms are typed as Unicode characters, like the Windows host does.
- Conversion from framebuffer pixels to global display points handles Retina scale factors, displays positioned left of or above the main display (negative origins), and fractional scales. A converted point lands inside the target pixel.
- A click-count tracker supplies `kCGMouseEventClickState`. It counts a press as a repeat of the previous one when it uses the same button within the system double-click interval and within a few pixels of it. Every rule above has tests that run on any platform.

### macOS host (`topvnc --serve` and the Server tab)

- Requires macOS 12.3 or later. On older systems the server refuses to start with a message saying so.
- **Permissions.** Before capture starts, the host checks Screen Recording access (`CGPreflightScreenCaptureAccess`).
  - If access is missing, it calls `CGRequestScreenCaptureAccess` and refuses to start. The error names *System Settings → Privacy & Security → Screen & System Audio Recording* and says the app must be relaunched after access is granted.
  - Accessibility access is checked with `AXIsProcessTrustedWithOptions` (prompting once). If it is missing, the server still starts **view-only**. It reports that state persistently, drops remote key and pointer events, and starts injecting them as soon as access is granted, with no restart.
  - When the server runs from a terminal, both permissions belong to the terminal app. The error messages say so.
- **Display selection.** The host serves the main display (`CGMainDisplayID`), or the display chosen with `--display NUMBER`, which is 1-based and follows `SCShareableContent` display order. The display name shown in the UI and reported in `ServerNotice::Serving` comes from the display's localized name when one is available.
- **Capture.**
  - An `SCStream` captures the display at its native **pixel** size (from the display mode's pixel width and height, not points), in `kCVPixelFormatType_32BGRA`.
  - The minimum frame interval is 1/60 s and the queue depth is small and fixed.
  - Frames are read from the IOSurface with its row stride through the existing `CaptureSurface` copy path.
  - The served name is `TopVNC macOS Desktop`.
- **Damage.**
  - Only frames with status `complete` are processed, and idle frames produce no update.
  - The dirty rectangles in a frame's info dictionary are validated, clipped to the display, and passed as damage regions. A complete frame without dirty rectangles is treated as fully damaged.
- **Cursor.** ScreenCaptureKit composites the cursor into the frames (`showsCursor`), so the backend does not read cursor shapes. Cursor movement reaches viewers through dirty rectangles.
- **Frame hand-off.**
  - The ScreenCaptureKit callback runs on its own dispatch queue and passes frames to the serving loop through a single-slot hand-off: a newer frame replaces an unread one, and damage from the replaced frame is merged in rather than lost.
  - No queue grows without bound, and the callback never blocks on the RFB server lock.
- **Recovery.**
  - When the stream stops with an error (display disconnected, permission revoked, capture interrupted), the host keeps serving the last image, reports the pause, and recreates the stream with the same backoff the Windows host uses.
  - A change in the display's pixel size, whether from a resolution change, a scale change, or a reconnect, is detected through a display reconfiguration callback. The stream is then recreated at the new size, and DesktopSize clients receive it as spec 004 requires.
- **Pointer input.** Remote pointer events are posted to `kCGHIDEventTap` on a dedicated input thread.
  - Moves become mouse-moved events, or dragged events while a button is held, at absolute global points.
  - Left, middle, and right buttons are supported, with click state for double and triple clicks.
  - RFB buttons 4–7 become line-unit scroll-wheel events (vertical on wheel 1, horizontal on wheel 2).
  - Pointer coordinates use the display placement that is current when the event arrives.
- **Keyboard input.**
  - Key events are posted with mapped key codes. The backend tracks held modifiers and sets the event flags explicitly on every key event, so a modifier held by one viewer applies to that viewer's keystrokes no matter how the system tracks synthetic modifiers.
  - Unicode fallback characters are posted with `CGEventKeyboardSetUnicodeString`.
  - Disconnecting a viewer releases only that viewer's keys and buttons, and shutdown releases everything still held.
- **Clipboard.**
  - The host polls `NSPasteboard.generalPasteboard` change count. New local plain text (`public.utf8-plain-text`) is published to viewers as Latin-1.
  - Viewer clipboard text is written to the pasteboard as a string, and the resulting change count is recorded so the text is not echoed back.
- **Port conflict.** If binding fails because the address is in use, the error says that macOS Screen Sharing or Remote Management may already be listening on port 5900 and suggests another port.

### Server tab

- On macOS the tab starts and stops the macOS host. The Windows-only status (`STOPPED / WINDOWS HOSTS ONLY`) stays only on platforms with no backend.
- While the server is stopped, the tab shows Screen Recording and Accessibility permission state. While it serves, a view-only state is reported as a persistent warning, like an unauthenticated server.
- All other behavior follows spec 004: form validation, unsaved passwords, the background thread, viewer counts, and releasing input on close.

## Limitations

- **Protocol and security:** RFB 3.8, Raw and Tight encodings (spec 006), standard VNC authentication, no TCP encryption, and Latin-1 clipboard limits are inherited from spec 004.
- **Bandwidth:** serving native pixels on Retina displays means large updates. A 5K display is 5120×2880, so a full Raw update is about 59 MB at 32 bits per pixel; Tight with JPEG (spec 006) reduces this, but the pixel count still scales encoding time and bandwidth.
- **Login and lock screen:** a user process cannot capture or drive the login window. While the Mac is locked, viewers keep the last image, and the screen cannot be unlocked through this server.
- **macOS prompts:** newer macOS versions show a recording indicator and may periodically ask the user to confirm Screen Recording access. The server cannot suppress either.
- **Keyboard layouts:** printable keys are mapped to ANSI US-layout key codes. Other layouts may produce different characters for mapped keys, the same limitation as the Windows host.
- **Media keys:** play, pause, next, and previous are not injected.
- **Sleep:** the host does not prevent display or system sleep.

## Validation plan

Done only when each item is checked on a real Mac and recorded here:

1. Build, `cargo test`, and `cargo clippy --all-targets` pass on `aarch64-apple-darwin`. Windows-target clippy still passes.
2. Permission flow from a fresh state:
   - Denied Screen Recording refuses to start, with the stated message.
   - Granting it and relaunching works.
   - Missing Accessibility gives a view-only server, which starts injecting once access is granted.
3. Connect TopVNC's viewer and one third-party viewer (for example TigerVNC) on a second machine, then check:
   - The image is correct on a Retina display and on an external display.
   - `--display 2` serves the second display.
   - Typing, modifiers, shortcuts such as Command-Tab and Command-C, drag, double-click, and both scroll axes all work.
   - Clipboard text syncs in both directions.
4. Change resolution and scale while serving, then unplug an external display while it is served, and record what DesktopSize and non-DesktopSize viewers see.
5. Lock and unlock the Mac locally while a viewer is connected, and record what the viewer sees and whether capture resumes.
6. Record CPU use and update rate for an idle desktop and for full-screen video, so later encoding work has a baseline.

## Implementation notes

- Keys without a direct Mac equivalent map to the key in the same position on an Apple keyboard: Insert to Help, Print, Scroll Lock, and Pause to F13–F15, and Num Lock to keypad Clear. Keypad navigation keysyms (sent with Num Lock off) map to the navigation keys.
- Key events carry the flags an Apple keyboard sets: numeric-pad for the keypad and arrows, and Fn for function and navigation keys. Modifier flags include the device-dependent left/right bits. Caps Lock events carry the system's current lock state instead of a held flag. Mouse events carry the same modifier flags, so shortcuts like Command-click work.
- Two presses form a multi-click when they are at most 4 framebuffer pixels apart on each axis.
- The stream queue depth is 3: one frame in the hand-off slot, one being copied, and one being captured. Captured pixel buffers stay retained until they are copied, so ScreenCaptureKit cannot reuse their surfaces early.
- Besides the reconfiguration callback, the serving loop checks the display's pixel size once a second. This covers processes whose main run loop does not deliver the callback promptly. `topvnc --serve` serves from a background thread and runs the main run loop.
- ScreenCaptureKit lists no displays while the Mac is locked. Starting the server then fails with a message saying the Mac may be locked or its display asleep. While serving, recreating capture keeps failing with that message until the Mac is unlocked.
- `build.rs` weak-links ScreenCaptureKit. Without this, the binary would not launch on macOS before 12.3, so it could never report the version requirement.
- Long Server tab messages, such as permission errors, wrap below the status panel.

## Validation status

Implemented; live validation is in progress. Recorded on 2026-10-01 on an M3 Max MacBook Pro, macOS 26.5.1:

1. **Done.** `cargo build`, `cargo test` (36 library tests and 47 binary tests), and `cargo clippy --all-targets -- -D warnings` pass on `aarch64-apple-darwin`. Windows-target clippy (`cargo xwin clippy --target x86_64-pc-windows-msvc --all-targets -- -D warnings`) passes, and the Windows build links. The platform-neutral rules (key ownership for both key types, the Mac key table and flags, pixel-to-point conversion, click counting, dirty-rectangle validation, frame hand-off) are tested on any platform. macOS-only tests cover dirty-rectangle decoding from Foundation dictionaries, the port-conflict message, the version check, the reconfiguration callback filter, and the shutdown signal handler. `otool -l` shows ScreenCaptureKit as `LC_LOAD_WEAK_DYLIB`. Offscreen renders of the Server tab were checked for stopped permission state, a wrapped permission error, and the view-only warning.
2. Not yet checked. Screen Recording and Accessibility were already granted to the terminal used, so the fresh-state flow was not exercised.
3. Not yet checked.
4. Not yet checked.
5. Partly checked: with the Mac locked and its display asleep, `topvnc --serve` reported `no display is available to capture; the Mac may be locked or its display asleep` and exited with status 1. Locking and unlocking while a viewer is connected has not been checked.
6. Not yet checked.
