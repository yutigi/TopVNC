#requires -Version 5.1
<#
.SYNOPSIS
Read-only Windows listener and firewall diagnostics for TopVNC.
.DESCRIPTION
Does not connect to peers, change networking, elevate, or alter firewall rules.
A listener or a readable firewall policy does not prove remote connectivity.
#>
[CmdletBinding()]
param([ValidateRange(1, 65535)][int]$Port = 5900)

function Get-TopVncBindMeaning {
    param([string]$Address)
    $parsed = $null
    if ([Net.IPAddress]::TryParse($Address, [ref]$parsed) -and [Net.IPAddress]::IsLoopback($parsed)) {
        return 'Loopback only; another computer cannot connect to this listener.'
    }
    switch ($Address) {
        '0.0.0.0' { return 'All IPv4 interfaces; clients must use a concrete server IP, never 0.0.0.0.' }
        '::' { return 'All IPv6 interfaces; IPv4 acceptance depends on the socket configuration.' }
        default { return 'Only this local address; clients need a route to this address.' }
    }
}

function Get-TopVncNetworkReport {
    param([ValidateRange(1, 65535)][int]$Port = 5900)
    $listeners = @()
    $listenerState = 'Unknown'
    $listenerDetail = ''
    try {
        # Enumerate first: a successful empty result differs from access denied.
        $listeners = @(Get-NetTCPConnection -State Listen -ErrorAction Stop |
            Where-Object { $_.LocalPort -eq $Port } | ForEach-Object {
                [pscustomobject]@{
                    BindAddress = $_.LocalAddress
                    Port = $_.LocalPort
                    ProcessId = $_.OwningProcess
                    Meaning = Get-TopVncBindMeaning $_.LocalAddress
                }
            })
        $listenerState = 'Known'
        $listenerDetail = 'Verify ProcessId belongs to TopVNC; another application may own this port.'
        if ($listeners.Count -eq 0) { $listenerDetail = 'No listener was observed on this port. Start the server or check its configured port.' }
    } catch { $listenerDetail = 'Listener state unknown: ' + $_.Exception.Message }

    $firewallState = 'Unknown'
    $firewallDetail = ''
    $profiles = @()
    try {
        $profiles = @(Get-NetFirewallProfile -PolicyStore ActiveStore -ErrorAction Stop |
            Select-Object Name, Enabled, DefaultInboundAction)
        # Reading profiles alone does not establish permission to inspect rules.
        $null = @(Get-NetFirewallRule -PolicyStore ActiveStore -ErrorAction Stop)
        $firewallState = 'Readable'
        $firewallDetail = 'Policy was readable. This helper does not evaluate effective access, block rules, or managed policy precedence.'
    } catch { $firewallDetail = 'Firewall state unknown (not absent): ' + $_.Exception.Message }

    [pscustomobject]@{
        Port = $Port
        ListenerState = $listenerState
        ListenerDetail = $listenerDetail
        Listeners = $listeners
        FirewallState = $firewallState
        FirewallDetail = $firewallDetail
        FirewallProfiles = $profiles
        NextStep = 'On the client, test the concrete server private IP and port. A successful self-test does not prove the peer route or inbound access. A timeout alone does not identify the firewall as the cause.'
    }
}

if ($MyInvocation.InvocationName -ne '.') { Get-TopVncNetworkReport -Port $Port }
