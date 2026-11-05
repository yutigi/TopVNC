# Session handover: UI for installing/removing the TopVNC Host service

Created 2026-11-05 21:45 +0700 (SEAST) · repo `D:\FreetimeProject\AI\TopVNC\.claude\worktrees\display-recovery-and-lock-screen` (a git worktree of `D:\FreetimeProject\AI\TopVNC`) · branch `fix/display-recovery-and-lock-screen` (clean, pushed; this handover file under `out/` is untracked) · Windows 11, Warp terminal, Git Bash + PowerShell 7

## Goal

Add a GUI for what is currently CLI-only: installing the TopVNC Host service (`topvnc --install-service <same options as --serve>`, which must run from an administrator terminal) and removing it (`topvnc --uninstall-service`). The user should be able to install and remove the service from the TopVNC window without opening an admin terminal. "Done" means you can install, see the service status, and uninstall from the UI, with tests, docs, and the same security guarantees as the CLI.

## Where things stand

- Done and committed in `c942f18`, PR https://github.com/yutigi/TopVNC/pull/8 (open, not merged):
  - **Issue 1, display recovery (spec `specs/019-display-recovery/spec.md`):** Displays without a connected monitor are filtered out (`connected_sources()` in `src/windows_server.rs`). The follower restarts once on a stale listing (`DisplayFollower::layout_read` in `src/desktop_host.rs`). A new `TVNC` capture-status pseudo-encoding (`CaptureStatus` in `crates/topvnc-rfb/src/lib.rs`) is shown by the browser (`web/app.mjs` `showCaptureStatus`) and the native viewer title (`src/main.rs`).
  - **Issue 2, Windows host service (spec `specs/020-windows-host-service/spec.md`):** `src/windows_service.rs` is new. Main entry points:
    - `install(arguments)`: validates options through `service_arguments()`, refuses `--allow-insecure`, and allows plain TCP only on loopback. Requires admin (`administrator_needed()`). It then:
      1. prompts for the password twice with `rpassword` on the console;
      2. copies the exe to `%ProgramFiles%\TopVNC\topvnc.exe`;
      3. stores the password as the LSA secret `L$TopVNCHostPassword`;
      4. runs `CreateServiceW` for `TopVNCHost` (LocalSystem, auto start);
      5. starts it and prints a summary, plus a hint about the SoftwareSASGeneration policy.
    - `uninstall()`: stops the service and waits, runs `DeleteService`, deletes the secret, and removes the copied exe.
    - `run_service()` (`--host-service`, run by the service manager) and `run_agent()` (`--host-agent`, SYSTEM in the console session).
    - CLI dispatch is in `src/main.rs` `main()` (match on the first argument).
  - **Desktop following (agent mode only):** `follow_input_desktops()` / `follow_input_desktop()` in `src/windows_server.rs`. Ctrl+Alt+Delete is detected by `is_secure_attention()` in `src/desktop_host.rs` and sent through `SendSAS` in the service.
- **Verified:**
  - `cargo fmt --all`, `cargo clippy --workspace --all-targets` (0 warnings), and `cargo test --workspace` all pass.
  - Web tests (`cd web && node --test tests/*.test.mjs`, listed in `web/package.json`) and the wasm build pass.
  - Non-admin smoke tests of every service CLI entry point were run, and nothing was installed (`sc query TopVNCHost` returns error 1060).
- **Not verified (unticked in the PR test plan):** a physical monitor unplug; installing the service and testing lock, unlock, sign-in, UAC, Ctrl+Alt+Del, and user switching; macOS clippy and tests on a Mac (`ring` needs a C compiler for the darwin cross-check here).
- **In progress:** nothing. The UI work has not started.

## Decisions and constraints

- On 2026-11-05 the user chose to build service mode, an opt-in SYSTEM service, over a spec-only option. The security posture is recorded in spec 020 under **Security** and must hold for the UI too:
  - The service always requires a password.
  - Plain TCP only on loopback.
  - The password is never on a command line, in a file, or in a log.
  - The service only runs the Program Files copy, never a user-writable path.
  - TopVNC never changes the SoftwareSASGeneration policy itself.
- Desktop following is gated to the agent. The Server tab and `--serve` behaviour must stay unchanged.
- **Existing UAC elevation pattern to reuse or learn from:** `src/helpers.rs:317-395`. `elevation_code()` runs an encoded PowerShell command through `Start-Process -Verb RunAs` and returns output through a unique pre-created file. The Firewall tab's Set and Reset buttons use it (see `Action::changes_firewall()`). Tests mock `Start-Process` so UAC is never triggered (`helpers.rs:564`).
- **UI code:**
  - The Server tab is drawn by `server_tab()` at `src/ui.rs:1504`; `enum Tab` is at `src/ui.rs:343`, and the WEB tab is `web_tab()`.
  - The form is `ServeForm`, rendered with `config.serve`.
  - The hosted server state is `struct HostedServer` at `src/main.rs:713`, with event handling in `show_landing()` at `src/main.rs:888`.
  - The UI is a custom wgpu/winit canvas (5x7 glyphs, uppercase labels), not native widgets.
- The repo writes CRLF in the working tree. A substitution script that assumes LF will miss matches; the Edit tool works fine.
- In this worktree session, compound Bash commands containing `git` or `sed`-with-substitution were sometimes refused by the isolation guard. Plain single commands work.
- The user's memory note says to work in the main checkout unless they ask for a worktree. This worktree exists because they asked.

## Next steps

1. **Focus: implement the UI for install and remove.** Decide the password hand-off first; see the open questions below. The rest of the plan:
   - Add an install/remove control and a status readout (installed, running, stopped, not installed) to the Server tab, or to a new tab.
   - Query the status without admin rights: `OpenSCManagerW(SC_MANAGER_CONNECT)` + `OpenServiceW(SERVICE_QUERY_STATUS)`, reusing `ScHandle` in `src/windows_service.rs`.
   - Install with the Server tab's current options, converted to `--serve` arguments. Note that `--allow-insecure` is refused, and plain TCP only works on loopback.
   - Show the SoftwareSASGeneration hint (`software_sas_allowed()`) and the log path in the UI.
   - Disable the Server tab's Start while the service is listening on the same port, or explain the conflict.
2. Add focused tests (argument building from `ServeForm`, status mapping, an elevation command with mocked `Start-Process`). Never trigger real UAC in tests.
3. Update the README section "Unlocking a locked Windows host" and spec 020 (add UI acceptance criteria before building, as AGENTS.md requires).
4. Run `cargo fmt --all`, `cargo test --workspace`, `cargo clippy --workspace --all-targets`, and the web tests. Commit to the same branch and update PR #8 only after the user confirms.

## Open questions / waiting on the user

- **How does the elevated installer get the password without exposing it?** `Start-Process -Verb RunAs` cannot redirect stdin, and the password must not go on a command line or into a file. Options to put to the user:
  - **(a)** The elevated `topvnc --install-service --password-pipe <name>` reads it from a named pipe that the unelevated GUI creates, with a DACL limited to the current user and SYSTEM/Administrators.
  - **(b)** The elevated process shows its own console prompt, as today, opened in a visible admin console.
  - **(c)** The GUI asks only for confirmation and the elevated process prompts.
- Where the controls go: in the Server tab or a separate SERVICE tab.
- Whether the user wants to live-test the service (spec 020 criteria 2–8) before more UI work.

## Guard rails

- Re-read `AGENTS.md`, `.specify/memory/constitution.md`, and the user memory before acting; they override this handover.
- Approvals from the previous session do not carry over. Confirm with the user again before:
  - pushing commits, or updating or merging PR #8;
  - installing the service or triggering UAC on this machine.

  The previous session never installed the service; on 2026-11-05 the user asked only to commit and open the PR.
- `out/` is not gitignored in this repo: do not commit `out/handover/`.
