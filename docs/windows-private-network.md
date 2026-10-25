# Windows private-network diagnostics

The diagnostic and firewall helpers use Windows PowerShell 5.1+ and the built-in
NetTCPIP and NetSecurity commands. They do not start TopVNC, change its bind address,
configure a VPN, or automatically elevate. No dependency installation is needed.
The separate work-server launcher starts the existing debug executable. All
addresses below are fictional examples; replace them with your own configuration.

The app's **Work**, **Network**, and **Firewall** tabs expose these helpers.
Work opens an independent terminal for the launcher's password prompt. Network
shows a read-only report; Firewall previews the exact scope and prints commands
for manual use in an existing administrator PowerShell. Reports have Previous /
Next pages and a Copy report button. Review deployment details before sharing
copied output. Neither diagnostic tab changes policy or automatically elevates.

## Work/home startup order

1. Connect your private VPN on both computers and allow remote access from the
   intended peer. Ensure any workplace-managed network policy permits the flow.
2. On the work computer, in an interactive desktop session, set the local bind
   address explicitly and run the launcher:

   ```powershell
   $env:TOPVNC_BIND = '192.168.50.10:5900' # Replace with your local VPN IPv4 and port
   .\scripts\Start-TopVncWorkServer.cmd
   ```

   Without `TOPVNC_BIND`, the launcher uses loopback (`127.0.0.1:5900`), which
   accepts only local connections. Enter the VNC password locally and keep the
   terminal open. Closing it or pressing Ctrl+C stops hosting. Passwords are
   neither saved nor passed on the command line. VNC uses only the first eight
   password bytes. The launcher neither builds nor elevates or changes policy.

   Build with `cargo build` if `target\debug\topvnc.exe` is missing. Run
   `Start-TopVncWorkServer.cmd /check` to check the path without starting a server.
3. On the other computer, test the configured private address and port:

   ```powershell
   Test-NetConnection 192.168.50.10 -Port 5900 # Replace the example address
   ```

   Check `TcpTestSucceeded`, then connect the viewer to that address with the
   password entered on the server. Ping alone does not establish TCP access.

Review existing firewall scope before proposing a new rule. An allow rule may
still be overridden by managed policy or a VPN filter. The launcher does not
control VPNs, firewall rules, audit settings, or execution policy. TopVNC traffic
is unencrypted; retain a trusted private transport. Keep actual deployment
addresses, machine names, and firewall-rule identifiers out of shared runbooks.

## Inspect the server

From the server's ordinary PowerShell session:

```powershell
.\scripts\Test-TopVncNetwork.ps1 -Port 5900 | ConvertTo-Json -Depth 5
```

This command only queries local listeners and firewall policy; it sends no peer
probe. Verify the reported process ID belongs to TopVNC. A listener on
`127.0.0.1` or `::1` accepts only local connections. `0.0.0.0` means every IPv4
interface, **not** a destination to enter in the client. Use the server's concrete
private-network address in the client. An IPv6 wildcard listener does not by
itself prove IPv4 acceptance.

`Unknown` means a query failed, including when access is denied. It does not mean
there are no rules or no listener. Even readable firewall policy does not prove
effective access: this helper does not compute block-rule precedence, managed
policy, routing, or VPN peer permissions.

From the **other computer**, you can explicitly test TCP reachability:

```powershell
Test-NetConnection -ComputerName 192.168.50.10 -Port 5900
```

A successful test on the server itself does not establish remote reachability.
A timeout can involve routing, VPN permissions, filtering, or a stopped server;
it does not identify the firewall as the cause. TCP success proves only TCP
reachability, not RFB authentication, desktop capture, or remote input.
TopVNC's TCP traffic is unencrypted: use a trusted private network or secure tunnel.

## Preview a scoped firewall proposal

Only if an inbound allow rule is appropriate, run this **on the server**. Start
without `-Apply` and inspect every field in `Parameters`:

```powershell
$scope = @{
    PeerAddress = '192.168.50.20'       # The other computer, never a subnet
    LocalAddress = '192.168.50.10'      # Address assigned to this server interface
    InterfaceAlias = 'PrivateVPN'      # Exact alias, not a wildcard
    ProgramPath = 'C:\TopVNC\topvnc.exe' # Actual server executable, not cargo.exe
    Profile = 'Public'                # One applicable Windows firewall profile
    Port = 5900
}
.\scripts\Set-TopVncPeerFirewall.ps1 @scope | ConvertTo-Json -Depth 4
```

The preview says **no changes made** and prints complete `ApplyCommand` and
`RollbackCommand` lines using the validated values and the helper's full path.
Review the scope, then copy the desired line into an existing administrator
PowerShell. The helper never executes the printed commands or opens UAC.
You can also keep the result and display either command as a plain string:

```powershell
$preview = .\scripts\Set-TopVncPeerFirewall.ps1 @scope
$preview.ApplyCommand
$preview.RollbackCommand
```

At an interactive `ProgramPath:` prompt, enter `C:\Program Files\TopVNC\topvnc.exe`
as plain text; spaces need no quoting there. One matching outer pair, such as
`"C:\Program Files\TopVNC\topvnc.exe"`, is also accepted. Interior characters are
preserved. In an inline command, PowerShell itself needs quotes around a path
with spaces, for example `-ProgramPath 'C:\Program Files\TopVNC\topvnc.exe'`.
For an apostrophe inside a single-quoted inline value, double it:
`-ProgramPath 'C:\Team O''Brien\topvnc.exe'`. The printed commands quote these
values for you.

The proposal restricts inbound TCP to one executable, local IP, remote peer IP,
port, interface, and profile, with edge traversal blocked. Public can be an
appropriate profile for a private VPN interface; choose the actual applicable
profile. The helper does not change the interface's profile. RFC1918 and CGNAT
IPv4 hosts are accepted. DNS, IPv6, public addresses, CIDR ranges, lists, wildcard
aliases, loopback, and final octets `0`/`255` are deliberately unsupported.
Some valid host addresses ending in `0` or `255` are therefore rejected.

If policy or interface inspection is denied, no rule is assumed missing and no
change is made. Re-run the preview in an administrator PowerShell to inspect it;
the script never prompts for elevation. Preview does not require administrator
rights when the system permits the necessary reads.

After review, an operator can explicitly apply the same scope in an existing
**administrator PowerShell**. Keep the same executable file, interface, and scope
available for rollback:

```powershell
.\scripts\Set-TopVncPeerFirewall.ps1 @scope -Apply -WhatIf # Preview mutation
.\scripts\Set-TopVncPeerFirewall.ps1 @scope -Apply         # Explicit change
.\scripts\Set-TopVncPeerFirewall.ps1 @scope -Remove        # Preview rollback
.\scripts\Set-TopVncPeerFirewall.ps1 @scope -Remove -Apply # Explicit rollback
```

The deterministic helper rule name includes a hash of the full scope. Reapplying
an exact rule or removing an absent rule is a no-op. An existing helper name with
different scope causes an error; the script never updates or removes that rule.
Rollback removes only the exact helper rule, leaving other rules intact. Changed
scope produces a different rule name, so retain the original arguments to remove
the original rule. This is a local PersistentStore rule; managed policy and
blocking rules can still prevent access. Re-test from the peer after an approved
change. This helper cannot certify that a connection problem is fixed.

## Tests and validation limits

With Pester already installed, run from the repository root:

```powershell
Import-Module Pester
Invoke-Pester -Script .\scripts\tests\Network.Tests.ps1
```

The tests mock OS queries, administrator checks, and firewall writes. They cover
default read-only behavior, bind explanations, denied reads, input validation,
quoted prompted paths, copy-ready command replay, exact rule scope, explicit
apply, `WhatIf`, idempotence, and guarded rollback.
They neither change a firewall nor certify the Windows firewall provider's live
behavior. Real administrator apply/rollback, managed policy interaction, remote
TCP, and end-to-end VNC require separate operator validation. If local execution
policy blocks scripts, follow your organization's policy; the helpers do not
change or bypass it. Review local paths, process IDs, and addresses before sharing
diagnostic output.

The work-server launcher's `/check` mode is covered by a Rust integration test
using loopback; the test does not start a server. Current helper-tab validation
and remaining environment limits are recorded in `specs/006-script-tabs/spec.md`.
