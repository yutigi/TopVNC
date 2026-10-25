# Run with the already installed Pester (3.4+); no dependency download needed.
. "$PSScriptRoot\..\Test-TopVncNetwork.ps1"
. "$PSScriptRoot\..\Set-TopVncPeerFirewall.ps1" -PeerAddress 192.168.50.20 -LocalAddress 192.168.50.10 -ProgramPath C:\TopVNC\topvnc.exe -InterfaceAlias TestVPN -Profile Public

function New-TopVncTestFirewallRule {
    param([hashtable]$Parameters)
    # The NetSecurity pipeline requires this CIM class even when commands are
    # mocked. Construct a client-side object only; no CIM operation is performed.
    $rule = New-Object Microsoft.Management.Infrastructure.CimInstance 'MSFT_NetFirewallRule', 'root/standardcimv2'
    # NetSecurity exposes Name as an alias of InstanceID and enum getters over
    # numeric CIM fields. Match that schema rather than creating display fields.
    $properties = @{
        InstanceID = [string]$Parameters.Name
        Description = [string]$Parameters.Description
        Profiles = [uint16][Microsoft.PowerShell.Cmdletization.GeneratedTypes.NetSecurity.Profile]$Parameters.Profile
        Enabled = [uint16][Microsoft.PowerShell.Cmdletization.GeneratedTypes.NetSecurity.Enabled]$Parameters.Enabled
        Direction = [uint16][Microsoft.PowerShell.Cmdletization.GeneratedTypes.NetSecurity.Direction]$Parameters.Direction
        Action = [uint16][Microsoft.PowerShell.Cmdletization.GeneratedTypes.NetSecurity.Action]$Parameters.Action
        EdgeTraversalPolicy = [uint16][Microsoft.PowerShell.Cmdletization.GeneratedTypes.NetSecurity.EdgeTraversal]$Parameters.EdgeTraversalPolicy
    }
    foreach ($property in $properties.Keys) {
        $rule.CimInstanceProperties.Add([Microsoft.Management.Infrastructure.CimProperty]::Create(
            $property, $properties[$property], [Microsoft.Management.Infrastructure.CimFlags]::None))
    }
    return $rule
}

Describe 'Read-only listener diagnostics' {
    BeforeEach {
        Mock Get-NetTCPConnection { @() }
        Mock Get-NetFirewallProfile { [pscustomobject]@{ Name = 'Public'; Enabled = $true; DefaultInboundAction = 'Block' } }
        Mock Get-NetFirewallRule { @() }
        Mock New-NetFirewallRule { throw 'Unexpected mutation' }
        Mock Remove-NetFirewallRule { throw 'Unexpected mutation' }
    }
    It 'distinguishes wildcard bind from a usable client destination' {
        (Get-TopVncBindMeaning '0.0.0.0') | Should Match 'never 0.0.0.0'
        (Get-TopVncBindMeaning '::') | Should Match 'depends on the socket'
        (Get-TopVncBindMeaning '127.0.0.1') | Should Match 'another computer cannot'
        (Get-TopVncBindMeaning '127.0.0.2') | Should Match 'another computer cannot'
        (Get-TopVncBindMeaning '::1') | Should Match 'another computer cannot'
        (Get-TopVncBindMeaning '192.168.50.10') | Should Match 'Only this local address'
    }
    It 'reports only listeners for the selected port and keeps process ownership explicit' {
        Mock Get-NetTCPConnection {
            [pscustomobject]@{ LocalAddress = '0.0.0.0'; LocalPort = 5900; OwningProcess = 42 }
            [pscustomobject]@{ LocalAddress = '127.0.0.1'; LocalPort = 5901; OwningProcess = 99 }
        }
        $report = Get-TopVncNetworkReport
        $report.ListenerState | Should Be 'Known'
        $report.Listeners.Count | Should Be 1
        $report.Listeners[0].ProcessId | Should Be 42
        $report.ListenerDetail | Should Match 'another application'
        $report.NextStep | Should Match 'timeout alone does not identify the firewall'
    }
    It 'keeps denied listener reads unknown rather than claiming no listener' {
        Mock Get-NetTCPConnection { throw 'Access denied' }
        $report = Get-TopVncNetworkReport
        $report.ListenerState | Should Be 'Unknown'
        $report.ListenerDetail | Should Match 'Access denied'
    }
    It 'distinguishes a successful empty listener query' {
        $report = Get-TopVncNetworkReport
        $report.ListenerState | Should Be 'Known'
        $report.ListenerDetail | Should Match 'No listener was observed'
    }
    It 'keeps denied rule reads unknown even when profiles are readable' {
        Mock Get-NetFirewallRule { throw 'Access denied' }
        $report = Get-TopVncNetworkReport
        $report.FirewallState | Should Be 'Unknown'
        $report.FirewallDetail | Should Match 'not absent'
    }
    It 'does not equate readable policy with effective access' {
        $report = Get-TopVncNetworkReport
        $report.FirewallState | Should Be 'Readable'
        $report.FirewallDetail | Should Match 'does not evaluate effective access'
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled Remove-NetFirewallRule -Times 0 -Exactly -Scope It
    }
}

Describe 'Scoped firewall proposal and explicit application (all OS calls mocked)' {
    BeforeEach {
        $script:options = @{
            PeerAddress = '192.168.50.20'; LocalAddress = '192.168.50.10'
            ProgramPath = 'C:\TopVNC\topvnc.exe'; InterfaceAlias = 'TestVPN'
            Profile = 'Public'; Port = 5900
        }
        Mock Test-Path { $true }
        Mock Get-NetIPAddress { [pscustomobject]@{ IPAddress = '192.168.50.10'; InterfaceAlias = 'TestVPN' } }
        Mock Get-NetFirewallRule { @() }
        Mock New-NetFirewallRule {
            $script:createdRuleParameters = @{
                RemoteAddress = [string]$RemoteAddress; LocalAddress = [string]$LocalAddress
                Program = [string]$Program; InterfaceAlias = $InterfaceAlias
                Profile = [string]$Profile; LocalPort = [string]$LocalPort; Protocol = [string]$Protocol
                Direction = [string]$Direction; Action = [string]$Action; PolicyStore = [string]$PolicyStore
            }
        }
        Mock Remove-NetFirewallRule { }
        Mock Test-TopVncAdministrator { $true }
        Mock Get-NetFirewallApplicationFilter { [pscustomobject]@{ Program = $script:options.ProgramPath } }
        Mock Get-NetFirewallPortFilter { [pscustomobject]@{ Protocol = 'TCP'; LocalPort = '5900' } }
        Mock Get-NetFirewallAddressFilter { [pscustomobject]@{ LocalAddress = '192.168.50.10'; RemoteAddress = '192.168.50.20' } }
        Mock Get-NetFirewallInterfaceFilter { [pscustomobject]@{ InterfaceAlias = 'TestVPN' } }
    }
    It 'defaults to preview and includes every narrow scope' {
        $result = Invoke-TopVncPeerFirewall @options
        $result.Applied | Should Be $false
        $result.Status | Should Be 'Preview - no changes made'
        $result.AdministratorInstruction | Should Match 'administrator PowerShell'
        $result.ApplyCommand | Should Match ' -Apply$'
        $result.RollbackCommand | Should Match ' -Remove -Apply$'
        $result.Existing | Should Be $false
        $result.Parameters.RemoteAddress | Should Be '192.168.50.20'
        $result.Parameters.LocalAddress | Should Be '192.168.50.10'
        $result.Parameters.Program | Should Be 'C:\TopVNC\topvnc.exe'
        $result.Parameters.InterfaceAlias | Should Be 'TestVPN'
        $result.Parameters.Profile | Should Be 'Public'
        $result.Parameters.LocalPort | Should Be 5900
        $result.Parameters.Direction | Should Be 'Inbound'
        $result.Parameters.EdgeTraversalPolicy | Should Be 'Block'
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled Remove-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled Test-TopVncAdministrator -Times 0 -Exactly -Scope It
    }
    It 'rejects broad, public, malformed, and injected peer addresses' {
        foreach ($bad in @('Any', '*', '0.0.0.0', '192.168.50.0/24', '192.168.50.0', '192.168.50.255',
            '8.8.8.8', '127.0.0.1', '224.0.0.1', '169.254.1.1', '100.63.1.1', '100.128.1.1',
            '172.15.1.1', '172.32.1.1', '192.168.50.256', '192.168.050.20', '10.1',
            '192.168.50.20,192.168.50.21', '192.168.50.20; Write-Error injected', '::1', 'example.test')) {
            $options.PeerAddress = $bad
            { Invoke-TopVncPeerFirewall @options } | Should Throw
        }
        Assert-MockCalled Get-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'accepts canonical RFC1918 and CGNAT hosts' {
        foreach ($address in @('10.1.2.3', '172.16.1.2', '172.31.1.2', '192.168.1.2', '100.64.1.2', '100.127.1.2')) {
            { Assert-TopVncPrivateHost $address } | Should Not Throw
        }
    }
    It 'rejects unsafe local address, self peer, wildcard interface, invalid port and profile' {
        $options.LocalAddress = '0.0.0.0'
        { Invoke-TopVncPeerFirewall @options } | Should Throw
        $options.LocalAddress = $options.PeerAddress
        { Invoke-TopVncPeerFirewall @options } | Should Throw
        $options.LocalAddress = '192.168.50.10'
        $options.InterfaceAlias = 'Test*'
        { Invoke-TopVncPeerFirewall @options } | Should Throw
        $options.InterfaceAlias = 'TestVPN'
        $options.Port = 0
        { Invoke-TopVncPeerFirewall @options } | Should Throw
        $options.Port = 5900
        $options.Profile = 'Any'
        { Invoke-TopVncPeerFirewall @options } | Should Throw
    }
    It 'rejects missing, relative, UNC or non-executable application paths' {
        foreach ($bad in @('topvnc.exe', '\\server\share\topvnc.exe', 'C:\TopVNC\script.ps1')) {
            $options.ProgramPath = $bad
            { Invoke-TopVncPeerFirewall @options } | Should Throw
        }
        $options.ProgramPath = 'C:\TopVNC\missing.exe'
        Mock Test-Path { $false }
        { Invoke-TopVncPeerFirewall @options } | Should Throw
    }
    It 'normalizes one matching outer quote pair on prompted paths only' {
        $options.ProgramPath = '"C:\Team O''Brien\topvnc.exe"'
        $doubleQuoted = Invoke-TopVncPeerFirewall @options
        $doubleQuoted.Parameters.Program | Should Be "C:\Team O'Brien\topvnc.exe"
        $doubleQuoted.ApplyCommand | Should Match "C:\\Team O''Brien\\topvnc.exe"
        $options.ProgramPath = "'C:\Team O'Brien\topvnc.exe'"
        $singleQuoted = Invoke-TopVncPeerFirewall @options
        $singleQuoted.RuleName | Should Be $doubleQuoted.RuleName
        $options.ProgramPath = "C:\Team O'Brien\topvnc.exe"
        (Invoke-TopVncPeerFirewall @options).RuleName | Should Be $doubleQuoted.RuleName
        $options.ProgramPath = '"C:\TopVNC\topvnc.exe'
        { Invoke-TopVncPeerFirewall @options } | Should Throw
        $options.ProgramPath = '""C:\TopVNC\topvnc.exe""'
        { Invoke-TopVncPeerFirewall @options } | Should Throw
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'replays both commands with exactly preserved values without evaluating path content' {
        # A harmless local capture function replaces the script endpoint. No
        # firewall helper is executed by these replay tests.
        function Capture-TopVncReplay {
            param([string]$PeerAddress, [string]$LocalAddress, [string]$ProgramPath,
                [string]$InterfaceAlias, [string]$Profile, [int]$Port,
                [switch]$Apply, [switch]$Remove)
            [pscustomobject]$PSBoundParameters
        }
        $scope = $options.Clone()
        $scope.ProgramPath = 'C:\Team O''Brien\$(throw ''injected''); #\topvnc.exe'
        $scope.InterfaceAlias = 'Private VPN (2)'
        $commands = New-TopVncReplayCommands -ScriptPath 'Capture-TopVncReplay' -Scope $scope
        $applyReplay = & ([scriptblock]::Create($commands.ApplyCommand))
        $removeReplay = & ([scriptblock]::Create($commands.RollbackCommand))
        foreach ($key in @('PeerAddress', 'LocalAddress', 'ProgramPath', 'InterfaceAlias', 'Profile', 'Port')) {
            $applyReplay.$key | Should Be $scope[$key]
            $removeReplay.$key | Should Be $scope[$key]
        }
        $applyReplay.Apply | Should Be $true
        $applyReplay.PSObject.Properties.Name -contains 'Remove' | Should Be $false
        $removeReplay.Remove | Should Be $true
        $removeReplay.Apply | Should Be $true
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled Remove-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'quotes the script path itself and generates only one command' {
        $path = 'C:\Tool O''Brien\Set-TopVncPeerFirewall.ps1'
        $commands = New-TopVncReplayCommands -ScriptPath $path -Scope $options
        foreach ($command in @($commands.ApplyCommand, $commands.RollbackCommand)) {
            $tokens = $null; $errors = $null
            $ast = [System.Management.Automation.Language.Parser]::ParseInput($command, [ref]$tokens, [ref]$errors)
            @($errors).Count | Should Be 0
            $calls = @($ast.FindAll({ param($node) $node -is [System.Management.Automation.Language.CommandAst] }, $true))
            $calls.Count | Should Be 1
            $calls[0].CommandElements[0].Value | Should Be $path
        }
    }
    It 'refuses a stale local IP and interface pair' {
        Mock Get-NetIPAddress { @() }
        { Invoke-TopVncPeerFirewall @options } | Should Throw
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'fails closed on denied firewall enumeration even with Apply requested' {
        Mock Get-NetFirewallRule { throw 'Access denied' }
        { Invoke-TopVncPeerFirewall @options -Apply } | Should Throw
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled Remove-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'requires administrator rights for Apply and never tries elevation' {
        Mock Test-TopVncAdministrator { $false }
        { Invoke-TopVncPeerFirewall @options -Apply } | Should Throw
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled Remove-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'respects WhatIf even with Apply' {
        $result = Invoke-TopVncPeerFirewall @options -Apply -WhatIf
        $result.Applied | Should Be $false
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'creates exactly one scoped rule when explicitly applied' {
        $result = Invoke-TopVncPeerFirewall @options -Apply -Confirm:$false
        $result.Applied | Should Be $true
        Assert-MockCalled New-NetFirewallRule -Times 1 -Exactly -Scope It
        $expected = @{
            RemoteAddress = '192.168.50.20'; LocalAddress = '192.168.50.10'
            Program = 'C:\TopVNC\topvnc.exe'
            Profile = 'Public'; LocalPort = '5900'; Protocol = 'TCP'
            Direction = 'Inbound'; Action = 'Allow'; PolicyStore = 'PersistentStore'
        }
        foreach ($key in $expected.Keys) { $script:createdRuleParameters[$key] | Should Be $expected[$key] }
        @($script:createdRuleParameters.InterfaceAlias).Count | Should Be 1
        $script:createdRuleParameters.InterfaceAlias[0].IsMatch('TestVPN') | Should Be $true
        $script:createdRuleParameters.InterfaceAlias[0].IsMatch('TestVPN extra') | Should Be $false
    }
    It 'uses a stable name and treats an exact existing rule as idempotent' {
        $proposal = Invoke-TopVncPeerFirewall @options
        $script:existing = New-TopVncTestFirewallRule $proposal.Parameters
        $script:existing.Name | Should Be $proposal.RuleName
        Mock Get-NetFirewallRule { $script:existing }
        @(Get-NetFirewallRule -PolicyStore PersistentStore).Count | Should Be 1
        (Get-NetFirewallRule -PolicyStore PersistentStore).Name | Should Be $proposal.RuleName
        $result = Invoke-TopVncPeerFirewall @options -Apply
        $result.RuleName | Should Be $proposal.RuleName
        $result.Applied | Should Be $false
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'rolls back only the exact matching helper rule' {
        $proposal = Invoke-TopVncPeerFirewall @options
        $script:existing = New-TopVncTestFirewallRule $proposal.Parameters
        Mock Get-NetFirewallRule { $script:existing }
        $result = Invoke-TopVncPeerFirewall @options -Remove -Apply -Confirm:$false
        $result.Applied | Should Be $true
        Assert-MockCalled Remove-NetFirewallRule -Times 1 -Exactly -Scope It -ParameterFilter { $InputObject.Name -eq $script:existing.Name }
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'treats repeated rollback as a no-op' {
        $result = Invoke-TopVncPeerFirewall @options -Remove -Apply
        $result.Applied | Should Be $false
        $result.Note | Should Match 'Already absent'
        Assert-MockCalled Remove-NetFirewallRule -Times 0 -Exactly -Scope It
    }
    It 'refuses removal or reuse of a name collision with broader scope' {
        $proposal = Invoke-TopVncPeerFirewall @options
        $script:existing = New-TopVncTestFirewallRule $proposal.Parameters
        Mock Get-NetFirewallRule { $script:existing }
        Mock Get-NetFirewallAddressFilter { [pscustomobject]@{ LocalAddress = 'Any'; RemoteAddress = 'Any' } }
        { Invoke-TopVncPeerFirewall @options -Apply } | Should Throw
        { Invoke-TopVncPeerFirewall @options -Remove -Apply } | Should Throw
        Assert-MockCalled New-NetFirewallRule -Times 0 -Exactly -Scope It
        Assert-MockCalled Remove-NetFirewallRule -Times 0 -Exactly -Scope It
    }
}
