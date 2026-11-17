# Notice when Windows blocks remote input

## Scope

Reported in [issue #19](https://github.com/yutigi/TopVNC/issues/19): after Task Manager was opened on a remote Windows desktop, clicks stopped working, and the session seemed to block all remote input. Expected: remote input keeps working, or, if the host cannot control Task Manager because of Windows permissions, a clear explanation and recovery guidance.

This covers two things. First, the server avoids the block: on Windows it asks for administrator approval and serves as administrator (see [Serving as administrator](#serving-as-administrator)). Second, when the server still runs without those rights, it explains the block to the host and to TopVNC's viewers. A process without administrator rights cannot control elevated apps; Windows does not allow it. The [host service](../020-windows-host-service/spec.md) runs as SYSTEM and can also reach UAC prompts and the lock screen. Changing which window is in front, and retrying key or button releases that Windows rejected, were considered and left out.

## Serving as administrator

The person asked for the fix to happen automatically, not only to be explained. Of the options, they chose to reopen TopVNC as administrator each time a server starts. The other options were a separate elevated input helper, and sending blocked input to the host service. The service option was rejected because any program the user runs could then drive administrator windows through it.

- **Window:** on Windows, when the window does not run as administrator, **Start server** (Server tab) and **Start browser access** or **Pair** (WEB tab) first check the form as before. The Server tab also checks that its TLS files load, and the WEB tab checks its files and that a password is set. The window then asks Windows for approval (`ShellExecuteEx`, `runas`) off the UI thread and shows that it is waiting. The approved process is `topvnc --elevated-window --ui-pipe NAME`. The window sends it a handoff over the private pipe from spec 020: the start to perform, the Server tab's form and password, and the window's own command line. TLS paths are made absolute first. The new window answers `ready`, performs the same start, and the first window closes. While waiting, the first window starts nothing else.
- The window reopens only when closing it stops nothing: no server running in it, no browser access (including one that uses the host service), no running helper, and no service installation in progress. Otherwise the server starts here, without administrator rights. A reopened window never reopens again.
- **Terminal:** `--serve` checks its options and asks for the password as before. It then starts `topvnc --serve --ui-pipe NAME OPTIONS` as administrator, without a window, and sends the password over the pipe, unless the options include `--allow-insecure`. The terminal shows the messages the server writes to the pipe and exits with its result. The elevated server watches the process that created the pipe and stops when it ends, such as on Ctrl+C or when the terminal closes, releasing held keys. `--no-elevate` skips all of this.
- **Without approval:** if approval is cancelled or fails, the same start runs without administrator rights, as before this change. A server without administrator rights reports this once it starts: "Running without administrator rights: while an app that runs as administrator, such as Task Manager, is in front, Windows blocks remote input." When an elevated app then blocks input, the host message suggests starting again with approval or installing the host service. A server that already runs as administrator suggests only the service.
- **Security:** approval needs someone at the host. Windows shows the prompt on the secure desktop, which an ordinary server cannot capture, so a remote viewer cannot approve it. The handoff pipe keeps the guarantees of spec 020: a random name, first instance only, remote clients rejected, and a DACL for SYSTEM, Administrators, and this account. The window checks that the client is the process it started, and the elevated side opens the pipe at identification level. The password is never on a command line, in a file, or in a log. The handoff is length-checked (at most 64 KiB), and an unknown setting, a missing start, or an out-of-range value is refused.
- **Scope of the rights:** the reopened window, its viewer, the browser gateway, and the tunnel it starts run as administrator.

## Findings

Not reproduced live here. Reading the code against Windows behavior:

- Task Manager always runs at high integrity (as administrator). User Interface Privilege Isolation (UIPI) stops a process from injecting input into a process with a higher integrity level. While such a window is in front, `SendInput` from the Server tab or `--serve`, which run at medium integrity, rejects every event. That includes mouse clicks aimed at other windows, so the user cannot click elsewhere to move focus away. Keyboard input is rejected the same way.
- Input is still sent for every event, so it works again as soon as the elevated app closes or someone at the host brings another window to the front.
- The server already reported `Windows rejected remote input` once, and `Remote input injection resumed` afterward, but only to the host's Server tab or terminal. The person at the viewer, who needs the explanation, was told nothing.
- Releases are rejected too. A key or button pressed before the elevated app came to the front, such as Ctrl and Shift after Ctrl+Shift+Esc, can stay down in Windows until it is pressed and released again.

## Acceptance criteria

### Host (Windows)

- When `SendInput` rejects input, the server checks the window in front. It looks up the owning process with `PROCESS_QUERY_LIMITED_INFORMATION`, and compares the process's mandatory integrity level with its own. A process that runs above the server's level, or whose token the server may not read, counts as more privileged.
- If a more privileged app is in front, the host message names its executable (for example `Taskmgr.exe`). It explains that Windows blocks all remote input until that app closes or another window is brought to the front, and suggests the TopVNC Host service. Other causes keep the existing message.
- The host is told each new cause once (status and app name), not on every rejected mouse movement, and is told when input is accepted again. The app check runs at most every 250 ms while input is rejected.
- The executable name is shown only on the host. Viewers get a status code only.

### Telling viewers

- New TopVNC private pseudo-encoding `0x54564e49` (TVNI): a payload-free rectangle with `x` = input status and `y` = width = height = 0. Status codes: 0 accepted, 1 blocked by a more privileged app in front, 2 rejected for another reason. Viewers treat unknown nonzero codes as 2. A rectangle with nonzero `y`, width, or height is a protocol error.
- Viewers advertise the encoding, and the host announces only to viewers that did, as soon as the status changes. Viewers start out assuming input is accepted. Other viewers are not told.
- The status becomes 0 as soon as input is accepted again. While it is 1, the host checks the app in front every 250 ms while idle, and announces 0 once that app is no longer more privileged, so the notice clears without the viewer first trying input. Status 2 clears only when input succeeds.
- The browser shows the explanation at the top of the picture, so the middle of the live picture stays visible, and in the status line. If capture is also stopped, the capture notice is shown instead, because a lock screen explains both. The native viewer adds `input blocked by an administrator app on the host` or `host rejects input` to the window title.

## Validation (Windows 11, 2026-11-17)

- Unit tests:
  - `input_status_codes_round_trip_and_unknown_codes_mean_rejected`, plus the replay and malformed-rectangle cases in `topvnc-rfb`: parsing, validation, and advertising.
  - `viewers_are_told_when_the_host_rejects_their_input_and_when_it_accepts_it_again`: a real server and native viewer exchange each status, with requested and pushed updates, and capture status stays live.
  - `the_page_learns_when_the_host_rejects_its_input`: the browser session raises the event only on change, maps unknown codes, and fails on malformed rectangles.
  - `an_elevated_app_in_front_is_told_apart_from_other_rejections`: integrity comparison, status choice, and host messages.
  - `viewers_and_the_host_learn_each_rejection_cause_once_and_when_input_resumes`: announcement and reporting rules, including clearing while idle.
  - `this_process_reads_its_own_integrity_level`: the token query works on this machine.
  - Serving as administrator. These tests never ask for approval:
    - `a_handoff_carries_the_server_form_and_the_viewer_command_line` and `an_empty_password_is_handed_over_for_allow_none`: every field round-trips, and TLS paths become absolute.
    - `malformed_handoffs_are_refused` and `handoffs_too_long_to_send_are_refused`.
    - `a_reopened_window_reads_its_frame_and_answers_the_window`: a thread stands in for the elevated window. It reads the frame, finds the window's process, and answers `ready`.
    - `frames_of_invalid_lengths_are_refused`, `terminal_servers_get_absolute_tls_paths`, and `windows_pipes_refuse_other_arguments`.
  - The existing installer pipe tests still pass with the shared pipe code.
- Smoke tests without administrator approval:
  - `--serve --no-elevate --allow-insecure --unencrypted 127.0.0.1:0` serves and prints the "Running without administrator rights" notice.
  - `--elevated-window` without a pipe, or with a path, is refused.
  - `--serve --ui-pipe NAME` with no window is reported as unreachable.
- Not yet validated: the approval flows need a person at the host.
  1. In the Server tab, click **Start server**, then approve. TopVNC reopens and serves with the same settings, and the first window closes. Repeat and cancel instead: the server starts in the first window with the notice.
  2. Run `--serve` from a terminal and approve. Messages appear in that terminal, and Ctrl+C stops the server.
  3. With the server running as administrator, open Task Manager from a viewer. Clicks and keys keep working in Task Manager and in other windows.
  4. Start browser access from the WEB tab and approve. The reopened window runs browser access.
- Also not yet validated: a live session with Task Manager in front. To check, start the Server tab or `--serve` without administrator rights, connect from the browser and from the native viewer, and open Task Manager on the host. Expect the notice in both viewers and a Server tab message naming `Taskmgr.exe`. Then close Task Manager, or click another window at the host. The notice should clear within about a second, and clicks should work again. Also check whether Ctrl or Shift stays down after opening Task Manager with Ctrl+Shift+Esc from the viewer.
