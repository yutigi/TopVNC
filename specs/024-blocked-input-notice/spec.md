# Notice when Windows blocks remote input

## Scope

Reported in [issue #19](https://github.com/yutigi/TopVNC/issues/19): after Task Manager was opened on a remote Windows desktop, clicks stopped working, and the session seemed to block all remote input. Expected: remote input keeps working, or, if the host cannot control Task Manager because of Windows permissions, a clear explanation and recovery guidance.

This covers explaining the block, to the host and to TopVNC's viewers. Making an ordinary server control elevated apps is out of scope: Windows does not allow it. The [host service](../020-windows-host-service/spec.md) already runs as SYSTEM and can. Changing which window is in front, and retrying key or button releases that Windows rejected, were considered and left out.

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
- Not yet validated: a live session with Task Manager in front. To check, start the Server tab or `--serve` without administrator rights, connect from the browser and from the native viewer, and open Task Manager on the host. Expect the notice in both viewers and a Server tab message naming `Taskmgr.exe`. Then close Task Manager, or click another window at the host. The notice should clear within about a second, and clicks should work again. Also check whether Ctrl or Shift stays down after opening Task Manager with Ctrl+Shift+Esc from the viewer.
