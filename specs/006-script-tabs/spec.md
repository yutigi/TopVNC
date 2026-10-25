# Windows helper tabs

## Scope

Add Work, Network, and Firewall tabs alongside Connect and Server for the three
helpers in `scripts/`. Keep protocol and desktop capture unchanged. Remove actual
deployment addresses, machine names, and rule identifiers from tracked examples;
users enter their own configuration at runtime.

## Acceptance criteria

- Work accepts a local IPv4 address and port, checks the existing debug executable
  with the launcher, and opens the launcher in a separate terminal only on an
  explicit start click. Password entry stays in that terminal. The tab explains
  that closing the terminal stops hosting, independently of the viewer window.
- Network accepts a port and runs the read-only diagnostic helper. Nested listener
  and firewall details and errors are readable in a paged output panel.
- Firewall accepts every scope parameter, validates the port and required fields,
  and provides Preview rule, Set rule, and Reset rule buttons. Preview and Enter
  are read-only. Set invokes explicit Apply; Reset invokes Remove + Apply for
  only the exact matching TopVNC rule and retains the form. Both request Windows
  administrator approval when needed; cancellation and failures appear in the
  report. The full apply and rollback commands remain copyable.
- Helpers run off the UI thread, with one outstanding operation, bounded output,
  a timeout, and no execution-policy changes. Only explicit Set/Reset clicks can
  request elevation. A timeout reports an uncertain rule state. Script paths
  resolve beside a distributed executable or in the source checkout.
- Tabs retain separate form/output state and keyboard focus. Windows-only actions
  show a platform explanation on other platforms.
- No actual deployment addresses appear in UI defaults, documentation, or tracked
  launch configuration. Runtime values are not saved.

## Validation

Run formatting, workspace tests, all-target Clippy, and the existing mocked
PowerShell helper tests. Inspect all five tabs in the Windows app, exercise
read-only diagnostics and launcher checks, and verify invalid forms. Do not start
a live server or apply firewall changes during UI verification.

## Validation results (2026-10-25, Windows)

### Set and Reset buttons

- Added explicit Set rule and Reset rule actions using the existing scoped
  helper. Reset removes only the exact matching rule and retains the form.
- Formatting and diff whitespace checks passed. All 55 binary tests passed,
  including scope validation and mocked elevation success, failure, and
  cancellation. No administrator prompt or firewall mutation was invoked.
- Workspace tests: 36 library tests passed; the previously recorded resize test
  still timed out at `src/lib.rs:2315`. All-target Clippy passed with the same
  three existing constant-chunk warnings.
- Built and visually inspected the Windows Firewall tab: Preview, Set, and Reset
  buttons and scope instructions fit without clipping. Live firewall changes
  and UAC approval were not exercised. Final build succeeded at
  `target/firewall-ui/debug/topvnc.exe`; the ordinary debug and previous UI-check
  executables were locked and could not be replaced. Documentation tests passed.
- Pester remains blocked by the existing script execution policy. No policy was
  changed or bypassed.

### Original helper tabs

- Formatting and diff whitespace checks passed. All 53 binary tests passed,
  including launcher check, actual PowerShell literal round-trip, field
  isolation, and report pagination tests. Documentation tests passed.
- Workspace testing: 36 library tests passed; the existing
  `clients_without_desktop_size_are_disconnected_on_resize` test timed out at
  `src/lib.rs:2315`, as previously recorded by the network-helper work.
- All-target Clippy passed with three existing constant-chunk suggestions in
  unchanged library/desktop-host code; no warnings in the added code.
- Built `target/ui-check/debug/topvnc.exe` because the ordinary debug executable
  was running. Inspected all five tabs in the Windows app and verified the final
  readable report layout. The preview did not start a server or change a rule.
- Pester could not load the helper test script because this environment's
  execution policy disables scripts. No policy was changed or bypassed. Live
  diagnostic/firewall preview execution remains unverified under that policy.
- Scanned tracked working files and new source/spec files against deployment
  addresses, hostnames, and rule identifiers from the previous runbook: no
  matches. The original network-helper commit was rewritten with a sanitized
  runbook and launcher before committing these tabs.
