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
