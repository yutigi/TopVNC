# Windows host service: remote lock and sign-in screens

## Scope

Reported: once the host locks, it cannot be controlled through the viewer, so a remote user cannot unlock it and continue. Wanted: where the operating system supports it, an authorized viewer reaches the normal lock or sign-in screen, authenticates there, and resumes. Operating system security boundaries are preserved: credentials are never bypassed and screen locking is never disabled.

In scope: Windows 10 and 11. Out of scope: macOS, where only Apple's Screen Sharing can drive the login window; Remote Desktop sessions; and running more than one host service.

## Why a service

Windows shows the lock screen, the sign-in screen, and UAC prompts on the Winlogon desktop. Only SYSTEM can open it, and a thread captures and sends input only on the desktop it is attached to. The Server tab and `--serve` run as the signed-in user, so they cannot see or type into these screens; that is a Windows boundary, not a TopVNC bug. Desktop Duplication of the secure desktop needs a SYSTEM process attached to it, and Ctrl+Alt+Delete needs `SendSAS` from a service. This is the approach Windows remote-support tools use.

## Design

- `topvnc --install-service [--serve options]` must run from an administrator terminal. It validates the options and TLS files, then asks for the VNC password twice. It copies the running `topvnc.exe` to `%ProgramFiles%\TopVNC\topvnc.exe`, stores the password, and creates and starts the `TopVNCHost` service: LocalSystem, automatic start, restart on failure after 5, 30, and 60 seconds.
- `topvnc --uninstall-service` stops and removes the service, deletes the stored password, and removes the copied executable.
- The service (`--host-service`, started by Windows only) has no desktop in session 0. It starts the agent (`--host-agent`) with its own SYSTEM token moved to the session at the physical console (`WTSGetActiveConsoleSessionId`), on `winsta0\default`. The agent is in a kill-on-close job, so it ends with the service. When another session comes to the console, the agent moves there. An agent that exits is restarted with backoff from 1 to 30 seconds, reset after a minute of running.
- The agent is the ordinary server. Before starting capture, each capture thread, and the input thread at least every 250 ms and after rejected input, attach to the desktop that receives input (`OpenInputDesktop`, `SetThreadDesktop`). Capture already restarts when the desktop switches, and now duplicates the new one. Following is enabled only in the agent; the Server tab and `--serve` are unchanged.
- Ctrl+Alt+Delete pressed in a viewer (Control and Alt held by any viewer, then Delete or keypad Delete) is not injected. The agent asks the service over its standard output, and the service calls `SendSAS(FALSE)` from `sas.dll`, loaded from System32 only. The service closes the agent's standard input to stop it gracefully, so held keys are released; after five seconds it ends the agent.
- The agent's notices go to the service log, `%ProgramData%\TopVNC\host-service.log`, which is rotated past 1 MiB.

## Security

- **Authentication:** the service always requires the VNC password; `--allow-insecure` is refused. Viewers then face the normal Windows lock or sign-in screen and its Windows password, PIN, or other sign-in method. Nothing types or stores Windows credentials.
- **Transport:** TLS (`--tls-cert`, `--tls-key`), or `--unencrypted` only on a loopback address, such as the browser gateway's target, which carries its own end-to-end encryption. Any other plain-TCP address is refused.
- **Password storage:** the password is stored as LSA private data (`L$TopVNCHostPassword`), which only SYSTEM can read back; storing it needs an administrator. It never appears on a command line, in the registry in clear text, in a file, or in the log. The agent keeps it in memory while serving, as `--serve` does.
- **Privilege:** the service runs only the copy under Program Files, which users cannot modify. Pointing a SYSTEM service at a user-writable build folder would let any local user run code as SYSTEM. The TLS key path is kept as given; keep it where only administrators can write.
- **Ctrl+Alt+Delete:** TopVNC never changes the *software Secure Attention Sequence* policy. The installer and the log say when it does not allow services; an administrator decides.
- **Clipboard:** clipboard sync continues on the lock screen. This lets an authenticated viewer paste into the password field, and it is the same text a viewer could already read before the screen locked.

## Acceptance criteria

1. Installing from a non-administrator terminal fails before anything changes, with a message saying how to run it as administrator. Refused options (`--allow-insecure`, or plain TCP on a non-loopback address) fail before the password prompt.
2. After installation, `sc query TopVNCHost` shows the service running, and a viewer can sign in with the VNC password and control the unlocked desktop as with `--serve`.
3. Lock the computer (Win+L) while a viewer is connected. Within a few seconds the viewer shows the lock screen, and a key press or click shows the sign-in prompt. Signing in with the account's Windows password or PIN returns to the desktop, which stays live and controllable.
4. After a restart, before anyone signs in, a viewer sees the sign-in screen and can sign in.
5. A UAC prompt on the secure desktop is shown and can be answered.
6. With the policy set to Services, Ctrl+Alt+Delete from a viewer opens the Windows security screen. With the policy unset, nothing happens and the log says why.
7. Switching users moves the agent to the new console session.
8. Stopping or uninstalling the service ends the agent, leaves no key held, and removes the password and the copied executable.

## Installing from the TopVNC window

Scope: install, see the status of, and remove the service from the TopVNC window, without opening an administrator terminal. The command line stays as it is, and both paths share one installer.

- **Where:** MORE ▸ SERVICE. The tab shows the service state: not installed, stopped, starting, running on its address, or stopping. It reads the state without administrator rights (`SC_MANAGER_CONNECT`, `SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG`) about once a second while the tab is shown. It also shows whether the Ctrl+Alt+Delete policy allows services, and where the service log is.
- **Options:** the service uses the Server tab's settings and password, in the same way as the WEB tab. The window checks them with the installer's rules before asking for approval: a password is required ("allow none" is refused), and plain TCP is allowed only on a loopback address.
- **Install:** clicking INSTALL starts `topvnc --install-service --ui-pipe NAME [--serve options]` hidden, through `ShellExecuteEx` with the `runas` verb, so Windows asks for administrator approval. The elevated installer reads the password from the pipe instead of the console, and writes its summary or error back to the pipe. The tab shows that text.
- **Remove:** REMOVE does the same with `topvnc --uninstall-service --ui-pipe NAME`. Nothing is sent to it.
- **Port conflict:** while the service is running, START SERVER in the Server tab refuses the service's port and says why.

Security of the password hand-off. The installer's guarantees above still hold:

- The window creates the pipe before it starts the installer. The pipe has these properties:
  - a random 128-bit name, `\\.\pipe\topvnc-service-<32 hex digits>`;
  - `FILE_FLAG_FIRST_PIPE_INSTANCE`, so no other process can already own the name;
  - one instance;
  - remote clients rejected;
  - a DACL that grants only SYSTEM, Administrators, and the signed-in user.
- The window sends the password only after it checks that the connected client is the process it started (`GetNamedPipeClientProcessId`). The password is length-prefixed and at most 1024 bytes.
- The elevated side accepts only a pipe name of exactly that form, so it never opens a path or a remote share. It opens the pipe at identification level (`SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION`), so the pipe's owner cannot impersonate the administrator.
- The password is never on a command line, in a file, or in a log. No console window is shown.

Acceptance criteria for the window:

9. Showing the SERVICE tab never asks for administrator approval. On a computer without the service, the tab shows NOT INSTALLED.
10. INSTALL is refused in the window, before any approval prompt, in these cases: an empty password, "allow none", or plain TCP on an address that is not loopback.
11. Cancelling the approval prompt installs nothing, and the tab says that approval was cancelled.
12. Approving installs and starts the service. The tab then shows the following, and no console window appears:
    - RUNNING ON the address;
    - the installer's summary;
    - the Ctrl+Alt+Delete policy state;
    - the log path.

    The password does not appear in `sc qc TopVNCHost` or in any file.
13. REMOVE, once approved, stops and removes the service, and the tab shows NOT INSTALLED.
14. With the service running on port P, START SERVER on port P is refused with an explanation, and another port still starts.

## Validation (Windows 11, 2026-11-05)

- Unit tests:
  - Service options: password required, plain TCP only on loopback, TLS paths made absolute.
  - Command-line quoting of spaces, quotes, empty arguments, and trailing backslashes.
  - Agent messages.
  - Ctrl+Alt+Delete detection from either side and the keypad, and not without both modifiers.
- Smoke tests from a non-administrator terminal:
  - `--host-service` explains that Windows starts it.
  - Refused options are reported before any prompt.
  - `--install-service` asks for an administrator terminal.
  - `--uninstall-service` reports that the service is not installed.
  - `--host-agent` cannot read the password outside SYSTEM.
  - Nothing was installed (`sc query TopVNCHost` fails with 1060).
- Not yet validated: criteria 2 to 8 need an administrator to install the service and a person at the computer to lock and unlock it. They have not been run.

Installing from the window (Windows 11, 2026-11-05). No test asks for administrator approval:

- Unit tests:
  - The service's options come from the Server tab's form and parse back the same. The password is never an argument. "Allow none", an empty password, and an invalid port are refused.
  - Before approval, plain TCP on a non-loopback address and missing TLS files are refused. Relative TLS paths reach the installer as absolute paths.
  - Command lines split back into the arguments they were built from. The installed command line gives the service's address.
  - Service manager states map to the tab's states. Reading the status succeeds without administrator rights.
  - Pipe names: only `topvnc-service-` plus 32 lowercase hex digits; never a path or a share.
  - Password frames round-trip. Empty, oversized, truncated, and non-UTF-8 frames are refused.
  - A window pipe with an installer standing in on a thread of the same process. The pipe name cannot be created twice. The password reaches the installer, and the installer's report and error come back.
  - A client that is not the started process is refused before anything is sent. An installer that ends without connecting is reported.
  - Port conflicts: any host on the service's port, only while it is starting or running.
  - The tab's layout, status lines, report wrapping, and its password shared with the Server tab.
- Smoke tests from a non-administrator terminal:
  - `--ui-pipe` with a name of the wrong form, or a path, is refused.
  - A well-formed name with no window is reported as unreachable.
  - `--uninstall-service extra` prints the usage.
  - Nothing was installed (`sc query TopVNCHost` fails with 1060).
- Not yet validated: criteria 9 to 14 in the running window. They need a person to look at the tab and to approve or cancel the prompt.
