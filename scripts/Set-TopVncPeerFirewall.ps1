#requires -Version 5.1
<#
.SYNOPSIS
Preview one narrowly scoped TopVNC IPv4 firewall rule. Changes require -Apply.
.DESCRIPTION
Run on the server. Default is read-only. Explicit -Apply requires an existing
administrator session; this script never elevates. -Remove -Apply rolls back
only an exact matching rule. Unknown policy state and name collisions fail closed.
#>
[CmdletBinding(SupportsShouldProcess)]
param(
    [Parameter(Mandatory)][string]$PeerAddress,
    [Parameter(Mandatory)][string]$LocalAddress,
    [Parameter(Mandatory)][string]$ProgramPath,
    [Parameter(Mandatory)][string]$InterfaceAlias,
    [Parameter(Mandatory)][ValidateSet('Domain', 'Private', 'Public')][string]$Profile,
    [ValidateRange(1, 65535)][int]$Port = 5900,
    [switch]$Apply,
    [switch]$Remove
)

function Assert-TopVncPrivateHost {
    param([string]$Address)
    # Canonical dotted decimal only: no DNS, CIDR, lists, wildcards, or shorthand.
    if ($Address -notmatch '^(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})\.(0|[1-9][0-9]{0,2})$') {
        throw 'Use one canonical private IPv4 host address, without a subnet or wildcard.'
    }
    $octets = @($Address.Split('.') | ForEach-Object { [int]$_ })
    if (@($octets | Where-Object { $_ -gt 255 }).Count -gt 0) { throw 'Invalid IPv4 octet.' }
    $private = $octets[0] -eq 10 -or
        ($octets[0] -eq 172 -and $octets[1] -ge 16 -and $octets[1] -le 31) -or
        ($octets[0] -eq 192 -and $octets[1] -eq 168) -or
        ($octets[0] -eq 100 -and $octets[1] -ge 64 -and $octets[1] -le 127)
    if (-not $private -or $octets[3] -eq 0 -or $octets[3] -eq 255) {
        throw 'Use an RFC1918 or CGNAT IPv4 host; final octets 0 and 255 are conservatively rejected.'
    }
}

function Test-TopVncAdministrator {
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    try {
        $principal = New-Object Security.Principal.WindowsPrincipal($identity)
        return $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
    } finally { $identity.Dispose() }
}

function New-TopVncReplayCommands {
    param([string]$ScriptPath, [hashtable]$Scope)
    # Single-quoted literals preserve spaces, apostrophes, $, backticks, and ;.
    # These strings are only displayed; this helper never evaluates them.
    $command = "& '" + $ScriptPath.Replace("'", "''") + "'"
    foreach ($parameter in @('PeerAddress', 'LocalAddress', 'ProgramPath', 'InterfaceAlias', 'Profile', 'Port')) {
        $literal = ([string]$Scope[$parameter]).Replace("'", "''")
        $command += " -" + $parameter + " '" + $literal + "'"
    }
    [pscustomobject]@{
        ApplyCommand = $command + ' -Apply'
        RollbackCommand = $command + ' -Remove -Apply'
    }
}

function Invoke-TopVncPeerFirewall {
    [CmdletBinding(SupportsShouldProcess)]
    param(
        [string]$PeerAddress, [string]$LocalAddress, [string]$ProgramPath,
        [string]$InterfaceAlias, [ValidateSet('Domain', 'Private', 'Public')][string]$Profile,
        [ValidateRange(1, 65535)][int]$Port = 5900,
        [switch]$Apply, [switch]$Remove
    )
    Assert-TopVncPrivateHost $PeerAddress
    Assert-TopVncPrivateHost $LocalAddress
    if ($PeerAddress -eq $LocalAddress) { throw 'PeerAddress must identify the other computer.' }
    if ($InterfaceAlias -notmatch '^[\p{L}\p{N} ._()-]{1,128}$') { throw 'Use one literal interface alias, without wildcard or control characters.' }
    # Mandatory-parameter prompts receive quote characters literally. Remove
    # exactly one matching outer pair; never trim or rewrite interior characters.
    if ($ProgramPath.Length -ge 2 -and
        (($ProgramPath[0] -eq [char]34 -and $ProgramPath[$ProgramPath.Length - 1] -eq [char]34) -or
         ($ProgramPath[0] -eq [char]39 -and $ProgramPath[$ProgramPath.Length - 1] -eq [char]39))) {
        $ProgramPath = $ProgramPath.Substring(1, $ProgramPath.Length - 2)
    }
    if ($ProgramPath -notmatch '^[A-Za-z]:\\' -or [IO.Path]::GetExtension($ProgramPath) -ne '.exe' -or
        -not (Test-Path -LiteralPath $ProgramPath -PathType Leaf)) { throw 'ProgramPath must be an existing local absolute .exe path.' }
    $program = [IO.Path]::GetFullPath($ProgramPath)
    # Refuse a stale or mistyped address/interface pair, before proposing anything.
    $addresses = @(Get-NetIPAddress -AddressFamily IPv4 -ErrorAction Stop |
        Where-Object { $_.IPAddress -eq $LocalAddress -and $_.InterfaceAlias -eq $InterfaceAlias })
    if ($addresses.Count -ne 1) { throw 'LocalAddress must be assigned to exactly this InterfaceAlias on the server.' }

    $scope = "$PeerAddress|$LocalAddress|$program|$InterfaceAlias|$Profile|$Port"
    $sha = [Security.Cryptography.SHA256]::Create()
    try { $hash = [BitConverter]::ToString($sha.ComputeHash([Text.Encoding]::UTF8.GetBytes($scope))).Replace('-', '').Substring(0, 24) }
    finally { $sha.Dispose() }
    $name = 'TopVNC-Peer-' + $hash
    $description = 'TopVNC peer helper v1; exact scope required for reuse or removal.'
    $ruleParameters = @{
        Name = $name; DisplayName = $name; Description = $description
        PolicyStore = 'PersistentStore'; Direction = 'Inbound'; Action = 'Allow'
        Enabled = 'True'; Protocol = 'TCP'; LocalPort = $Port
        RemoteAddress = $PeerAddress; LocalAddress = $LocalAddress
        Program = $program; InterfaceAlias = $InterfaceAlias; Profile = $Profile
        EdgeTraversalPolicy = 'Block'
    }
    # Enumerate with Stop. A denied read must never be treated as a missing rule.
    $existing = @(Get-NetFirewallRule -PolicyStore PersistentStore -ErrorAction Stop |
        Where-Object { $_.Name -eq $name })
    if ($existing.Count -gt 1) { throw 'Duplicate helper rule name; refusing changes.' }
    if ($existing.Count -eq 1) {
        $rule = $existing[0]
        $app = $rule | Get-NetFirewallApplicationFilter -ErrorAction Stop
        $ports = $rule | Get-NetFirewallPortFilter -ErrorAction Stop
        $address = $rule | Get-NetFirewallAddressFilter -ErrorAction Stop
        $interface = $rule | Get-NetFirewallInterfaceFilter -ErrorAction Stop
        $matches = $rule.Description -eq $description -and $rule.Direction -eq 'Inbound' -and
            $rule.Action -eq 'Allow' -and $rule.Enabled -eq 'True' -and [string]$rule.Profile -eq $Profile -and
            $rule.EdgeTraversalPolicy -eq 'Block' -and $app.Program -eq $program -and
            [string]$ports.Protocol -in @('TCP', '6') -and [string]$ports.LocalPort -eq [string]$Port -and
            [string]$address.LocalAddress -eq $LocalAddress -and [string]$address.RemoteAddress -eq $PeerAddress -and
            [string]$interface.InterfaceAlias -eq $InterfaceAlias
        if (-not $matches) { throw 'Helper rule name exists with different scope; refusing reuse or removal.' }
    }
    $operation = 'Create'
    if ($Remove) { $operation = 'Remove' }
    $commands = New-TopVncReplayCommands -ScriptPath $PSCommandPath -Scope @{
        PeerAddress = $PeerAddress; LocalAddress = $LocalAddress; ProgramPath = $program
        InterfaceAlias = $InterfaceAlias; Profile = $Profile; Port = $Port
    }
    $result = [pscustomobject]@{
        Status = 'Preview - no changes made'
        Operation = $operation; RuleName = $name; Existing = ($existing.Count -eq 1)
        Applied = $false; Parameters = $ruleParameters
        ApplyCommand = $commands.ApplyCommand
        RollbackCommand = $commands.RollbackCommand
        AdministratorInstruction = 'Review the scope, then copy the desired command into an existing administrator PowerShell. This script never launches UAC or runs these commands automatically.'
        Note = 'Preview only. Review scope, then run in an administrator PowerShell with -Apply. Use the same arguments with -Remove -Apply to roll back. Managed policy or block rules may still prevent access.'
    }
    if (-not $Apply) { return $result }
    $result.Status = 'No changes made'
    if (-not (Test-TopVncAdministrator)) { throw 'Apply requires an administrator PowerShell. No changes made; no elevation attempted.' }
    if ($Remove -and $existing.Count -eq 0) { $result.Note = 'Already absent; no change.'; return $result }
    if (-not $Remove -and $existing.Count -eq 1) { $result.Note = 'Exact rule already exists; no change.'; return $result }
    if ($PSCmdlet.ShouldProcess($name, $operation + ' scoped TopVNC peer firewall rule')) {
        if ($Remove) { $existing[0] | Remove-NetFirewallRule -ErrorAction Stop }
        else { $null = New-NetFirewallRule @ruleParameters -ErrorAction Stop }
        $result.Applied = $true
        $result.Status = 'Applied'
        $result.Note = 'Requested local rule change completed. Re-test from the peer; this is not proof of connectivity.'
    }
    return $result
}

if ($MyInvocation.InvocationName -ne '.') {
    $result = Invoke-TopVncPeerFirewall @PSBoundParameters
    if (-not $Apply) {
        Write-Host 'PREVIEW - no changes made.'
        Write-Host $result.AdministratorInstruction
        Write-Host 'Apply command (creates the scoped allow rule):'
        Write-Host $result.ApplyCommand
        Write-Host 'Rollback command (removes only the matching helper rule):'
        Write-Host $result.RollbackCommand
    }
    $result
}
