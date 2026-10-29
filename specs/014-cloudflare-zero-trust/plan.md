# Plan: E2E-first Cloudflare access to TopVNC

See [spec.md](spec.md) for confirmed requirements and acceptance criteria.
Recommend SSH between the viewer and actual host inside private Client-to-Tunnel
routing. This is a planning recommendation; no SSH installation, Cloudflare
changes, workplace contact, production code change, commit, or push is authorized
by this plan. Future execution needs authorization. Each checkpoint must pass
before advancing. Windows first, macOS second. All deployment examples are fictional.

## Current state and integration

- The user has a Cloudflare account and personally manages the intended host.
  E2E encryption is mandatory; retain VNC password authentication within it.
- Local smoke tests on `c9c33f5` passed Windows capture, password-only RFB,
  wrong-password rejection, and Raw/Tight full/incremental decoding. Input was
  inconclusive, and the probe never sent input. Listeners and fixtures were
  stopped. See the evidence in the spec; do not repeat interactive tests while
  someone else uses this desktop.
- Planning originated on `plan/cloudflare-zero-trust` at `c9c33f5`. Before the
  documentation commit, the branch was fast-forwarded to `main` at `2559871`,
  which already contains newer capture/display/session changes. Repeat applicable
  local tests on the selected deployment commit before implementation or rollout.
  A newer README is not test evidence; the old smoke run was not repeated.
- Feature number **014** replaces the draft's 013, reserved on main for
  multi-monitor. Keep the branch name. Spec Kit here resolves explicit feature
  context, rather than requiring a numbered branch. For read-only discovery use
  the following in the owned worktree; do not overwrite shared feature state:

  ```powershell
  $env:SPECIFY_FEATURE_DIRECTORY = 'specs/014-cloudflare-zero-trust'
  .\.specify\scripts\powershell\check-prerequisites.ps1 -PathsOnly -Json
  Remove-Item Env:SPECIFY_FEATURE_DIRECTORY
  ```

  Recheck number availability before future feature creation. Planning did not
  authorize changes to other worktrees; the user's subsequent cleanup request
  separately authorizes their removal with uncommitted work preserved. A future
  implementation needs its own agreed tasks; this document does not execute them.

## Phase 0: deployment inventory and trust preparation

1. Confirm the actual host OS and which interface owns the privately supplied
   address. Inventory existing SSH services/listeners/configuration/firewall rules
   and the account type available for a dedicated non-admin forwarding user.
   Windows discovery found a client, not a verified server installation. Do not
   infer that Windows optional features are installed or absent from PATH alone.
2. Identify the viewer, its management policy and existing Client organization.
   Confirm account plan/workload terms, enrollment permissions, and chosen device
   posture. Do not displace employer enrollment or circumvent network policy.
   A domain is unnecessary for the IP route. Keep deployment identifiers private.
3. Choose an isolated SSH configuration/instance if an existing service is used
   for other purposes. Preserve existing administration and recovery paths.
   Record before-state and exact rollback ownership for all proposed changes.
   Plan staging so an installer-created SSH firewall allow cannot expose the
   service even briefly: inspect installation/service behavior before execution,
   keep the service stopped until binds/configuration/default rules are verified.
   If staging cannot ensure this, stop and revise setup before installation.
4. Provision a dedicated viewer key (passphrase and OS permissions), copy only its
   public half to the dedicated host account through an authenticated channel,
   and protect host key/configuration/authorized-key files against unauthorized
   writes. Read the host public key/fingerprint at its console or over an existing
   independently authenticated management path. Transfer the public key to a
   dedicated viewer trust file and compare its SHA256 fingerprint independently.
   `ssh-keyscan` or an Access login alone is not independent verification.
5. Select unused example ports: host SSH **2222**, host loopback RFB **5900**,
   viewer loopback forward **15900**. Resolve conflicts rather than changing to
   wildcards. If RFB must use another loopback port, update every restriction,
   forward and test together; never add a direct RFB route.

Checkpoint: documented inventory, account/enrollment decisions, host-key trust
source, isolated SSH design, and rollback scope. No Cloudflare session yet.

## Phase 1: local E2E transport validation, Windows first

Only after setup/testing is authorized, validate a local SSH path before
Cloudflare is configured. Start TopVNC with a fresh temporary VNC password on
`127.0.0.1:5900`, without `--allow-insecure`. Keep actual credentials out of
arguments/logs. Finish isolated capture/input validation separately.

### Candidate Windows SSH server profile

Use a dedicated service/configuration or an explicitly reviewed equivalent that
preserves existing administration. This **untested candidate** assumes only the
forwarding account uses the dedicated instance. It is not a drop-in replacement
for a shared `sshd_config`:

```text
Port 2222
ListenAddress 127.0.0.1
AllowUsers topvnc_forward
PubkeyAuthentication yes
AuthenticationMethods publickey
PasswordAuthentication no
AllowTcpForwarding local
PermitOpen 127.0.0.1:5900
MaxSessions 0
PermitTTY no
AllowAgentForwarding no
GatewayPorts no
```

The intended combination is forwarding only to the exact RFB destination with
no shell/command/subsystem channels. See the [upstream server directives](https://man.openbsd.org/sshd_config).
For defense in depth, evaluate a per-key restriction on the single authorized
public key: `restrict,port-forwarding,permitopen="127.0.0.1:5900"`. This does not
replace the server's no-session and local-forward-only restrictions. Use the
[authorized-key documentation](https://man.openbsd.org/sshd.8) and test support.

Microsoft documents [Windows-specific configuration and unsupported options](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh-server-configuration).
Do not paste unsupported upstream `KbdInteractiveAuthentication`,
`AllowStreamLocalForwarding`, `PermitTunnel`, `PermitUserRC`, or `X11Forwarding`
into this Windows profile. Require public-key-only authentication and rejection
of unwanted channels in behavior, not an ignored directive. On macOS, separately
review those restrictions where available; do not copy the Windows profile and
assume equivalent behavior.

Use a lowercase standard local Windows account rather than an administrator;
Microsoft's [key guidance](https://learn.microsoft.com/en-us/windows-server/administration/openssh/openssh_keymanagement)
distinguishes standard-user and administrator key-file locations/ACLs and notes
that Entra ID accounts do not support this key-authentication path. Protect the
standard user's authorized-key file. Do not share an administrator key store.

Validate candidate configuration with the installed `sshd -t -f <candidate>` and,
where available, `sshd -T -f <candidate> -C <test-connection-context>` for effective
settings. Record executable version/path, effective settings, and startup output.
Parsing alone is insufficient. Unknown/unavailable behavior stops this path;
do not relax restrictions to obtain connectivity. Existing SSH configuration is
not replaced without a scoped migration and recovery plan.

### Candidate viewer profile

Use an isolated configuration file (example `~/.ssh/topvnc_config`) to avoid
inherited forwards and proxies. The trust file is populated **before** connecting,
with exactly the independently verified host key under `topvnc-actual-host`.
Both host-key database settings below use that same dedicated file.

```text
Host topvnc-cloudflare
    HostName topvnc-host.internal
    Port 2222
    User topvnc_forward
    HostKeyAlias topvnc-actual-host
    HostKeyAlgorithms ssh-ed25519
    UserKnownHostsFile ~/.ssh/topvnc_known_hosts
    GlobalKnownHostsFile ~/.ssh/topvnc_known_hosts
    StrictHostKeyChecking yes
    UpdateHostKeys no
    VerifyHostKeyDNS no
    IdentityFile ~/.ssh/id_ed25519_topvnc
    IdentitiesOnly yes
    PreferredAuthentications publickey
    PasswordAuthentication no
    ForwardAgent no
    ForwardX11 no
    GatewayPorts no
    LocalForward 127.0.0.1:15900 127.0.0.1:5900
    ExitOnForwardFailure yes
    ServerAliveInterval 15
    ServerAliveCountMax 3
    Compression no
```

This candidate pins an Ed25519 host key; confirm availability on the installed
build or review another modern key type and corresponding explicit pin. No weak
algorithm fallback. Inspect evaluated settings with `ssh -G -F <config> <alias>`.
For the local test use a separate alias/config with `HostName 127.0.0.1`; for E2
replace HostName with a fictional example private address such as `192.168.50.10`
in documentation, the actual address only in private local configuration.
The stable host-key alias identifies the actual host despite route/DNS changes.
Use modern supported SSH defaults for other algorithms and record the negotiated
algorithms. The chosen host key must match the pin; never use `accept-new`,
disable checking, or automatically replace a changed key.

A future operator starts the forwarding process using:

```text
ssh -F <isolated-config> -N -T topvnc-cloudflare
```

`-N -T` requests forwarding without a command or PTY; it does not impose server
restrictions on other clients. The [client manual](https://man.openbsd.org/ssh.1)
describes local forwarding. The [configuration manual](https://man.openbsd.org/OpenBSD-current/man5/ssh_config.5)
explains strict checking and warns that `ExitOnForwardFailure` covers forward
setup, not ultimate target reachability. Prove VNC authentication and decoded
updates through the forward; a local TCP listener is not proof of success.
TopVNC connects only to `127.0.0.1:15900` with the VNC password. Retain its
plaintext-transport warning because it cannot verify the external SSH process.
Keep the SSH process lifecycle explicit and stop it after the session.

### Required local rejection and cleanup tests

Record expected result, observed result, version, and attributable diagnostic.
Use disposable isolated keys/configuration, never alter a production trust pin
to manufacture a test. Check:

- Correct host pin, viewer key and VNC password succeed; wrong VNC password fails.
- Missing host pin and changed host key fail before any RFB channel is used;
  wrong/revoked viewer key and password-only SSH attempts fail.
- A shell, explicit command, SFTP/subsystem and PTY request fail. A request for
  remote forwarding fails. Attempts to forward another host/port fail; trigger a
  connection through a local forward to observe server-side rejection. Testing
  only whether `ssh -L` can bind a local socket proves no target restriction.
- A dynamic SOCKS request cannot relay to an unapproved destination. OpenSSH
  may create a local SOCKS listener even with destination restrictions; do not
  describe that listener alone as a bypass or promise its creation is denied.
- Viewer forward and TopVNC listen on IPv4 loopback only; inspect IPv6/wildcard
  listeners too. Another LAN device cannot connect to either listener.
- An occupied viewer port causes forward setup failure. A stopped TopVNC causes
  RFB failure even if SSH stays up. Stopping SSH drops the session and removes
  the local forward; TopVNC must not reconnect directly to a remote RFB port.

Checkpoint: Windows local SSH/RFB positive and negative checks pass with usable
diagnostics, no broad firewall allow, no leftover test listeners/keys/passwords.
Record remaining input limitations honestly. A same-machine test validates
composition only, not remote policy, route, LAN filtering, or latency.

## Phase 2: Cloudflare connector, policy and route experiments

Future authorized execution only, after Phase 1 passes:

1. Enroll the authorized viewer in the user's Zero Trust organization with
   owner-only enrollment and login policy. Use a dedicated narrow Include
   profile plus required Client/identity-service destinations. Do not install
   the Client on the host for this path. Establish policy before any route.
2. Prepare host `cloudflared` as an outbound service with its token stored only
   in protected local configuration. Installation history/transcripts may expose
   tokens; never put tokens in repository scripts or share service command lines.
   Verify permitted egress on 7844 UDP/QUIC or TCP/HTTP2 and record negotiation.
   Do not change managed egress policy to make it work. See [egress requirements](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/configure-tunnels/tunnel-with-firewall/).
3. Enable the Gateway TCP proxy; hostname routing also requires its documented
   UDP/DNS proxy settings. For an IP route use a dedicated virtual
   network if helpful for route isolation. It distinguishes Cloudflare routes;
   it does not rewrite addresses or fix conflicts with viewer LAN/VPN routes.
4. Install a scoped catch-all Block first, then a higher-priority owner identity
   Allow limited to TCP **2222**, then activate the route. Review existing rule
   precedence. Enrollment is a prerequisite; use an actual Passed Device Posture
   Checks selector if adopted. [Network policy selectors](https://developers.cloudflare.com/cloudflare-one/traffic-controls/network-policies/)
   support identity, destination, protocol, port and virtual-network scope.
   No Allow for 5900/15900, and no whole-LAN route. Include routing alone does
   not constrain ports.
5. Try **E1**, then **E2** only if needed. Neither is established:

   | Experiment | Candidate setup | Required evidence / stop condition |
   | --- | --- | --- |
   | E1: private hostname to host loopback SSH | `topvnc-host.internal` resolves to `127.0.0.1` in the connector service context; `sshd` stays on `127.0.0.1:2222`. Viewer gets a Gateway initial resolved IP. | Verify connector resolver behavior, actual-host pin and policy matching for SSH. Loopback origin is experimental; never route viewer `127.0.0.1/32`. If unsupported or matching cannot be proved, stop E1. |
   | E2: host private-address SSH | Route only the verified host private IPv4 `/32`; bind SSH to that single local address on 2222. TopVNC remains `127.0.0.1:5900`. | Prove same-host connector access while another LAN peer is denied, with no new inbound allow and inspection of existing rules. If both cannot coexist, stop and revise the design. Do not expose RFB or widen binds/firewall. |

   [Hostname routing](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/private-net/cloudflared/connect-private-hostname/)
   documents minimum connector/Client versions, service-side resolution and
   synthetic viewer addresses. SSH has no TLS SNI. Use a private Access app for
   only port 2222 and its documented Gateway private-app routing selector;
   verify denied identities and other ports without relying on SNI or DNS Allow.
   Consult [private-app guidance](https://developers.cloudflare.com/cloudflare-one/access-controls/applications/non-http/self-hosted-private-app/).
   The private-app selector alone is not a catch-all for ports outside that app.
   Before E1 activation, prove that its scoped Block covers all other ports and
   both address families across the actual initial resolved ranges and origin
   routes. If those ranges are shared and cannot be isolated without affecting
   unrelated apps, stop E1 and use E2. Do not claim a virtual network necessarily
   isolates hostname routes. Scope the routing Allow to this deployment as well
   as owner identity, TCP and 2222; Access still must authorize the user. Permit
   Client authentication notifications, complete the browser login, then retry
   SSH if needed. An Access timeout is not an SSH-key or VNC-password failure.
   If hostname scope cannot be proved, use the IP variant: owner User Email AND
   Destination IP host `/32` AND TCP AND port 2222; lower-priority scoped Block
   for everything else to the `/32` (or isolated virtual network).
6. Verify Include-mode route selection on every intended viewer network. For
   hostname E1, send its DNS to Gateway, not Local Domain Fallback. The hostname
   guide lists initial ranges explicitly; the current [Split Tunnels guide](https://developers.cloudflare.com/cloudflare-one/team-and-resources/devices/cloudflare-one-client/configure/route-traffic/split-tunnels/)
   says default IPv4/IPv6 ranges are managed automatically at runtime and custom
   ranges need inclusion. Inspect installed routes rather than copying stale
   ranges or assuming dashboard entries show them. Keep required control/DNS
   traffic reachable; Include mode is not literally only SSH payload bytes.
7. From another network, start pinned SSH and connect TopVNC to viewer loopback.
   Correlate allow and rejection evidence with Gateway/Access logs. Repeat local
   restriction tests over this path, including direct attempts to reach routed
   RFB and an approved temporary non-SSH test listener within the denied scope.
   A closed port or timeout cannot establish policy enforcement. Test unauthorized
   identity and unapproved/unenrolled device, accounting for any existing VPN
   route. Disabling the Client alone is not proof if another path still works.

Checkpoint: E2E, allowed owner path, denied identities/devices/ports and effective
listener/LAN scope established. Client/connector service recovers as intended;
host game traffic retains its existing path. Record Unknown/Not tested when
queries fail. The Windows network checker can inspect 5900 and 2222 separately,
but does not compute effective firewall policy; access denied is not absence.
macOS requires independent firewall inspection.

### Rollback experiment

Stop TopVNC and the viewer forward; disable this route/Access application and
stop the deployment-owned connector/SSH instance. Revert only its device profile,
policies, service/startup changes, keys/account, trust entries and E1 resolver
mapping using recorded before-state. If sharing an existing SSH service, remove
only the deployment's changes after checking existing access. Preserve unrelated
keys/rules and the prior VPN configuration. Remove any installer-created rule
owned by this deployment; never reset the whole firewall/organization profile.
Revoke exposed tokens and viewer keys when necessary. Verify no owned listener
remains, new Cloudflare access fails, and the prior approved VPN path works with
its original bind/authentication scope. Do not rebind RFB to a wildcard for rollback.

## Phase 3: measurement and disruption experiments

1. Future benchmark implementation must replace the current local-only timing
   assumptions in `examples/latency_bench.rs`. At `c9c33f5`, endpoints share
   in-process `Instant` timestamps, `--direct` is loopback, embedded/`--serve`
   modes allow insecure authentication, runs cap at 60 seconds, and output uses
   mean/p95. Do not route this benchmark unchanged or call it remote latency.
2. Design authenticated loopback host and remote viewer modes, obtaining a fresh
   VNC password with a masked local prompt or other reviewed secret input. Bind
   the synthetic host to `127.0.0.1:5900`, replacing TopVNC temporarily so SSH
   `PermitOpen` stays exact. Viewer connects through `127.0.0.1:15900`. Require
   password authentication even inside SSH; no insecure routed mode.
3. Time input and complete correlated returned marker on the viewer's clock;
   the synthetic server handles probes without OS input. Bound outstanding
   probes, identifiers, allocation, sample count and duration. Reject partial
   markers; report missing/coalesced probes. Test correlation/bounds, malformed
   markers, remote argument conflicts and authentication requirements. For
   authorized code changes run `cargo fmt --all`, `cargo test --workspace`, and
   `cargo clippy --workspace --all-targets` per AGENTS.md.
4. After agreeing the proposed latency gate and FPS tolerance, run at least
   three paired SSH-over-VPN / SSH-over-Cloudflare repetitions in alternating
   order on the same off-site topology. Use `tight6-push` and Ultra at 1920x1080,
   60/120 FPS, 300 measured seconds after warm-up once tooling supports it.
   Record synthetic timing boundaries, scene, offered/delivered rate, p50/p95/max
   intervals, bandwidth, missing probes, CPU and peak/steady memory including
   `ssh`, `sshd`, Client and connector. Keep SSH compression disabled in both.
   Compare existing VPN/TopVNC separately for SSH adoption cost; LAN is a separate
   reference. A same-host hairpin is not remote-topology evidence.
5. Run 60 uninterrupted minutes live play with the debug overlay; record drops
   and causes. Separately test SSH/connector restart, viewer network change, host
   sleep/lock, policy withdrawal, and key revocation. Removing an authorized key
   may not end an established SSH session: test new connections and explicitly
   terminate owned sessions for immediate revocation. Record manual recovery.
6. Record all software/build versions, host/viewer network, selected virtual
   network, tunnel protocol and Cloudflare colos from permitted diagnostics.
   Mark unavailable values Unknown; do not export credentials to collect them.
   Put sanitized results and gate decision in the spec. Distinguish synthetic
   decoding timing from real input-to-display measurements.

Checkpoint: report the agreed gate outcome and measured limits. No unsupported
claim that Cloudflare improves gaming performance or provides automatic reconnect.

## Phase 4: future documentation and read-only helpers

Only after relevant validation, create `docs/cloudflare-zero-trust.md` and link
it from README. Include independent key provisioning, exact scoped configurations,
loopback checks, lifecycle, failure behavior, negative tests, measurements and
rollback. Explain that TopVNC itself still uses plaintext loopback RFB. Describe
only tested platform pairs/configurations.

An optional future `scripts/Test-TopVncCloudflare.ps1` may query service state and
versions, SSH/RFB/forward binds, and readable firewall policy. It changes nothing,
prints no tokens/private keys/service command lines, and reports Unknown for
failed queries. It cannot attest SSH host verification from an open port alone.
Use the existing private-network runbook/checker's reporting conventions.

A conditional published-route appendix, if separately requested and permitted,
would carry SSH to host loopback 2222 through viewer `cloudflared access tcp`,
then use the same actual-host pin and loopback RFB forward. Never publish
`tcp://localhost:5900`. Check public-route workload terms and long-lived session
limitations; it is not a workaround for managed enrollment policy. No browser
renderer or Cloudflare-terminated SSH path is an E2E substitute.

## Later work requiring separate specifications

- Native TLS/VeNCrypt with certificate validation or pinning can replace external
  SSH after implementation and validation. `HandshakeStream`, server writes,
  `InputWriter` and `Session` currently use/clone `TcpStream`; design concurrent
  read/write and bounded backpressure explicitly. A mutex around one blocking
  TLS stream can stall input. Keep reusable RFB logic apart from platform
  transports and measure costs; do not defer required E2E while doing this work.
- Automatic reconnect needs backoff and unchanged host identity/security across
  retries; it must not bypass the SSH forward or select weaker authentication.
- A browser client needs a distinct E2E design terminating at the browser and
  actual host. Access JWT validation at a WebSocket gateway is additional access
  control, not payload confidentiality from that gateway. Settle hosting terms
  before a published implementation.

## Risks and unresolved decisions

| Risk or unknown | Resolution before deployment/adoption |
| --- | --- |
| Host OS/interface or viewer enrollment is unknown | Phase 0 inventory; Windows first, then independently validated macOS. |
| Windows OpenSSH restrictions differ from upstream | Phase 1 effective config and rejection tests; stop rather than weaken restrictions. |
| Existing/default SSH service or firewall exposure | Isolated staging and before-state; no service start with broad default allow. |
| E1 loopback origin or SSH hostname policy fails | Stop E1; test E2 for SSH only. Keep RFB loopback throughout. |
| E2 connector access requires an inbound allow | Stop and revise; do not silently relax the no-new-inbound-allow requirement. |
| IP overlaps local LAN/VPN | Inspect routes per network; virtual network selection is not address translation. |
| Relay interception, wrong origin or changed host key | Independently provisioned pin, strict checking, no relay-managed SSH termination. |
| Compromised local process or endpoint | Trusted endpoints are explicit; loopback and E2E do not isolate local attackers. |
| Edge distance, congestion or ordered-stream stalls | Paired encrypted measurements, CPU/memory accounting and separate long-session test. |
| Host lock/sleep or newer display behavior | Test exact deployment build; connector health cannot prove desktop capture/control. |
| Proposed performance gate is unapproved | Agree latency/FPS tolerance before measurement; keep VPN baseline. |
| macOS setup source unavailable | Apple source returned HTTP 429; recheck setup/restrictions and test actual macOS before claims. |
