# Infrastructure hardening plan

**Status:** Draft for CTO review, 2026-11-11. Item 0.3 is partly done; nothing
else in this plan is implemented yet.

This plan fixes the gaps found in the infrastructure review of 2026-11-11.
The code-level security work (Noise E2E, Access JWT checks in both the Worker
and the gateway, bounded buffers, loopback-only listeners) is sound and is not
redesigned here. The problems are in everything around the code: how changes
reach `main`, how builds become releases, how the Worker is deployed and rolled
back, how the host processes are kept alive, and how anyone finds out
something is broken.

## Findings and where they are fixed

| # | Finding | Severity | Plan item |
| --- | --- | --- | --- |
| 1 | `main` has no branch protection or ruleset; CI is advisory, and `deploy.mjs` trusts whatever is on `origin/main` | Critical | 0.1 |
| 2 | No release pipeline: last release v0.2.0 (macOS only), 96 commits since, no Windows release, unsigned binaries, no changelog | High | 1.1–1.4 |
| 3 | Production deploys run from a laptop with a broad Wrangler login; no staging; a failed post-upload check leaves the bad version live | High | 2.1–2.3 |
| 4 | The production path has never been run end to end; the live Worker is still the Phase 0 `topvnc-connection-spike` | High | 2.4, Phase 3 |
| 5 | No observability: one generic Worker log line, gateway logs only to a console window, no alerts | High | Phase 4 |
| 6 | No supervision of `cloudflared` and the gateway; `cloudflared` is unpinned with auto-update off | Medium | 5.1–5.3 |
| 7 | On Windows, `host.key` and `tunnel-token.txt` are protected only by a manual `icacls`; revocation means hand-editing `browsers.txt` | Medium | 5.4, 5.5 |
| 8 | No Dependabot, `cargo audit` or `cargo deny`; no `LICENSE` or `SECURITY.md` on a public repository | Medium | 0.2, 0.4, 0.5 |
| 9 | CI does not run macOS, `test:integration`, Pester, or the design tests | Medium | Phase 6 |
| 10 | Personal identifiers: the owner's account name (live `workers.dev` subdomain, wireframe URLs) in files and history, the personal email in commit metadata, and the host key fingerprint; five ignored `out/` files are tracked | Low | 0.3 |
| 11 | `docs/cloudflare-gateway.md` mixes runbook, status diary and security model in 900 lines | Low | 7.1 |
| 12 | `src/lib.rs` (6,846 lines) holds server code in the app crate, against the `AGENTS.md` layout | Low | 7.2 |

Correction to the review: `src/tight.rs` and `crates/topvnc-rfb/src/tight.rs`
are not duplicates. The first is the server's Tight **encoder**; the second is
the client **decoder**. Item 7.2 only moves the encoder.

## Decisions needed before work starts

Each decision blocks the item named. The recommendation is the default if no
one objects.

| Decision | Options | Recommendation | Blocks |
| --- | --- | --- | --- |
| License | MIT OR Apache-2.0 (Rust convention); GPL-3.0; proprietary ("all rights reserved") | MIT OR Apache-2.0, unless the code is meant to stay closed. In that case, make the repository private. | 0.2, 0.5 |
| Rewrite Git history to remove identifiers | Rewrite with `git filter-repo`; leave history and fix forward | **Decided 2026-11-11: rewritten.** The owner chose the rewrite over fixing forward, which also removed the personal email from commit metadata. See 0.3. | — |
| Where production deploys run | A: stay manual from the owner's machine; B: GitHub Actions with a protected `production` environment and a scoped API token | **B**, after 0.1 is in place. `wrangler login` grants an OAuth session with wide account scope on a laptop. A token limited to one account's Workers, held in an environment that needs manual approval, is narrower and leaves an audit log. This reverses the current "no deployment token in GitHub" choice, so the owner must agree. | 2.3 |
| Code signing | Windows: Azure Trusted Signing or an OV certificate. macOS: Apple Developer ID with notarization | Sign both. The Windows host service copies `topvnc.exe` into `Program Files` and runs it as SYSTEM, so an unsigned binary there is the biggest gap. Check current prices before buying. | 1.3 |
| Worker CPU on the Free plan | Stay on Free and accept the cold-start risk; Workers Paid | Stay on Free until the Phase 3 soak shows `exceededCpu`. If it does, the paid plan is the only full fix. | Phase 3 exit |
| Browser access when no one is logged in | Keep it tied to the WEB tab; run the gateway and `cloudflared` under the Windows host service | Host service. It already runs as SYSTEM with restart and backoff. This needs its own spec. | 5.2 |

## Phase 0: Quick wins (about 1 day)

### 0.1 Protect `main`

Add a repository ruleset on `main`:

- Require a pull request before merging. A solo owner cannot approve their own
  PR, so require 0 approvals and rely on status checks.
- Require these status checks, using the job names in `.github/workflows/ci.yml`:
  `Format`, `Rust (windows-latest)`, `Rust (ubuntu-latest)`,
  `Browser UI and reference goldens (Windows)`, and `Browser client and Worker`.
- Require branches to be up to date before merging.
- Block force pushes and branch deletion.
- Allow no bypass. CI takes about 4 minutes, so even emergencies can go through a PR.

**Acceptance:** a direct `git push origin main` is rejected, and a PR with a
failing required check cannot be merged.

### 0.2 License and security policy

- Add `LICENSE` (or `LICENSE-MIT` and `LICENSE-APACHE`) according to the decision above.
- Set `license` in every `Cargo.toml`, and `"license"` in `web/package.json`
  and `gateway/cloudflare/worker/package.json`.
- Add `SECURITY.md`. Turn on GitHub private vulnerability reporting and point to it.
  State the supported versions (latest release only) and the trust limit already
  written in the gateway docs: Cloudflare serves the page code.

**Acceptance:** GitHub shows the license on the repository page, and the
"Report a vulnerability" button is visible.

### 0.3 Remove personal identifiers and stray files

Partly done on 2026-11-11.

Done:

- History rewrite. `git filter-repo --sensitive-data-removal` replaced the
  owner's account name with `example` in every version of every file, which
  covered the live `workers.dev` address in `deploy.test.mjs` and the wireframe
  URLs. It also replaced the personal email in commit author and committer
  fields with the GitHub no-reply address. Every branch and tag was
  force-pushed. The first changed commit was the repository's first commit, so
  every commit ID changed.
- PR #17 points the wireframe links to the source under `design/`, stops
  tracking `out/`, and ignores it.
- The owner's checkout commits with the no-reply address.

Still open:

- Ask GitHub Support to dereference the 16 pull-request refs that still point
  at the old commits, and to garbage-collect them. Until then, the old commits
  still open by ID on GitHub.
- `specs/015-cloudflare-web-client/research.md`: replace the full host key
  fingerprint with `xxxx-…`.
- Add a CI step that fails when a tracked file is also ignored
  (`git ls-files -ci --exclude-standard` prints anything).
- Set the owner's global Git email to the no-reply address and turn on GitHub's
  email privacy settings, so other repositories stop exposing the personal
  address.

Clones made before the rewrite keep the old history, and nothing can change
that. Item 2.4 is the chance to retire the old subdomain.

**Acceptance:** searching the files and commit metadata of every branch and tag
for the owner's name and personal email finds nothing, the host key fingerprint
is not in tracked files, and `git ls-files -ci --exclude-standard` prints
nothing.

### 0.4 Dependency updates

Add `.github/dependabot.yml` with weekly, grouped updates for:

- `cargo` at `/` (the workspace);
- `npm` at `/web` and `/gateway/cloudflare/worker`;
- `github-actions` at `/`. The actions are SHA-pinned, so Dependabot keeps both
  the SHA and the version comment current.

**Acceptance:** Dependabot opens its first PRs, and they run the required checks.

### 0.5 Dependency policy in CI

- Add `deny.toml` with these checks:
  - `advisories`: deny known vulnerabilities, and warn on unmaintained crates;
  - `licenses`: allow only licenses compatible with the decision in 0.2;
  - `sources`: allow crates.io only;
  - `bans`: warn on duplicate versions.
- Add a `Dependencies` CI job that runs `cargo deny check`, using a pinned
  `cargo-deny` version, and make it a required check.

**Acceptance:** the job passes on `main`. Adding a crate with a known advisory
makes it fail.

## Phase 1: Release pipeline (about 1 week)

### 1.1 One version, and a changelog

- Move the version to `[workspace.package]` and give each crate
  `version.workspace = true`.
- Bump to `0.3.0`, which the README already mentions.
- Add `CHANGELOG.md` in Keep a Changelog form. Seed it from `git log v0.2.0..HEAD`,
  grouped into viewer, server, browser access, and infrastructure.

### 1.2 Release workflow

Add `.github/workflows/release.yml`, triggered by a `v*` tag:

| Artifact | Runner | Contents |
| --- | --- | --- |
| `topvnc-<ver>-windows-x64.zip` | `windows-latest` | `topvnc.exe`, `topvnc-web-gateway.exe` and `topvnc-tunnel.exe` side by side (the WEB tab expects the gateway next to TopVNC), `scripts/`, LICENSE, README |
| `topvnc-<ver>-macos-universal.dmg` | `macos-latest` | `lipo` of the aarch64 and x86_64 builds, `.app` bundle, gateway binaries |
| `SHA256SUMS` | any | Checksums of every artifact |

- Produce build provenance with `actions/attest-build-provenance`.
- Create a **draft** release; a person publishes it.
- Do not ship a Linux artifact until Linux is tested, as `AGENTS.md` requires.
- Recover how the v0.2.0 `.dmg` was built (Info.plist, icon, entitlements, any
  `create-dmg` steps). It was built outside the repository, so write that
  procedure into the workflow before you replace it.

### 1.3 Signing

- **Windows:** sign every `.exe` with Authenticode, using a timestamp server.
  The signing credentials live in a GitHub environment named `release`, which
  needs manual approval.
- **macOS:** codesign with Developer ID and the hardened runtime, notarize with
  `notarytool`, and staple the ticket to the `.dmg`.
- **Follow-up:** after a release is signed, have `--install-service` check the
  Authenticode signature of the binary it copies into `Program Files`
  (`WinVerifyTrust`). Write a spec before doing this.

### 1.4 Release checks

- Run the release workflow in build-only mode (no signing, no upload) on PRs that
  change `.github/workflows/release.yml`, `build.rs`, or the packaging files.

**Acceptance for Phase 1:** pushing tag `v0.3.0` produces a draft release with:

- a signed Windows zip, where `Get-AuthenticodeSignature` reports `Valid`;
- a notarized macOS `.dmg`, which `spctl -a -vv` accepts;
- the gateway binaries;
- `SHA256SUMS` and a provenance attestation.

## Phase 2: Deployment safety (about 1 week)

### 2.1 Staging Worker

The gateway accepts exactly one `app_origin`, so a staging Worker cannot reach
the production gateway. Staging needs its own gateway:

- A Worker named `topvnc-staging` with its own Access application for its own
  `workers.dev` hostname, and the same owner email.
- On one test host, a second gateway config with `listen = 127.0.0.1:6081`, the
  staging `app_origin`, its own host key, and its own tunnel and VPC service.
- `deploy.mjs` gets a `--target staging|production` option. Staging accepts any
  branch; production keeps the clean-`main` rule.
- Make `deploymentConfig` upstream validation target-specific. It currently
  accepts only `http://127.0.0.1:6080/ws`; staging must use
  `http://127.0.0.1:6081/ws` in `UPSTREAM_URL` (or each `HOSTS_JSON` upstream
  once multi-host routing is merged), matching the staging gateway and VPC
  service. Production keeps the exact `6080` URL. Select the expected URL from
  the target, and retain the loopback and exact-path checks.
- Add deployment-config tests that accept each target's expected URL and reject
  the other target's port, non-loopback addresses, and other paths.

**Acceptance:** an authenticated staging session reaches the gateway on `6081`,
production still reaches `6080`, and neither target accepts the other's upstream
configuration.

For changes to the page only, the existing local harness
(`cargo run -p topvnc-web-gateway --features test-fixtures --example local_web`)
remains the fast check.

### 2.2 Automatic rollback

In `gateway/cloudflare/worker/scripts/deploy.mjs`:

1. Before uploading, record the active version ID
   (`wrangler deployments list --json`).
2. Upload.
3. If the post-upload `verifyAccess` fails, run `wrangler rollback <previous-id>`,
   check `verifyAccess` again, and exit non-zero with both results.

Add tests with a mocked runner for each case: rollback succeeds, rollback fails,
and no earlier version exists (the first deploy).

**Acceptance:** a deliberately broken staging deploy rolls itself back, and the
command exits non-zero.

### 2.3 Deploy from CI (if decision B is taken)

Add `.github/workflows/deploy.yml`:

- Triggered by `workflow_dispatch`, and optionally by pushes to `main`, with a
  `staging` or `production` input.
- The `production` environment needs manual approval and is restricted to the
  `main` branch.
- Secrets per environment: `CLOUDFLARE_API_TOKEN`, scoped to one account and to
  Workers Scripts Edit plus whatever VPC bindings need (confirm the minimum scopes
  on staging first), and `CLOUDFLARE_ACCOUNT_ID`.
- The other config values (`ACCESS_AUD`, the VPC service IDs, `HOSTS_JSON`,
  `OWNER_EMAIL`) become environment variables. `deploy.mjs` builds the Wrangler
  config from them in place of `wrangler.local.toml`, and keeps the same
  `deploymentConfig` validation.
- The workflow reuses `deploy.mjs`, so the checks, Access verification and
  rollback are the same as a local deploy.
- Keep the manual path working as a fallback.

**Acceptance:** a production deploy waits for approval, and it is recorded in
the environment's history with the commit SHA.

### 2.4 Move production off the Phase 0 name

The live Worker is `topvnc-connection-spike`. Browser pairings live in
per-origin storage, so **changing the hostname means every browser pairs
again.** Plan the move as one window.

Consider changing the account's `workers.dev` subdomain in the same window.
Clones made before the 2026-11-11 history rewrite still contain the old one,
and every browser pairs again anyway.

1. Create an Access application for `topvnc.<subdomain>.workers.dev` and record
   its new AUD tag.
2. Deploy the Worker as `topvnc` with the new `APP_ORIGIN` and `ACCESS_AUD`.
   Change `app_origin` and `audience` in each host's `gateway.local.toml` to
   that origin and AUD tag, then restart the gateways. Both the Worker and
   gateway validate the audience, so changing only the origin breaks access.
3. Run `verifyAccess` against the new origin, then pair each browser again and
   confirm an authenticated desktop session works on each host. The redirect
   check alone does not exercise audience validation. Then try a different
   email at Access and confirm it is refused, which was never tested.
4. After a week without problems, delete the old Worker and its Access application.
5. Move any host still using `gateway/cloudflare/spike/host.key` or
   `spike/browsers.txt` to `gateway/cloudflare/`. Then delete the
   `gateway/cloudflare/spike` workspace and its `gateway`, `pair` and `tunnel`
   cargo aliases.

**Acceptance:** the runbook in `docs/cloudflare-gateway.md` has been run as
written, under the production name, and its verification table is updated with
the date.

## Phase 3: Live validation (owner, about 2 days)

These are the checks the docs list as never run. Record the results, with
dates, in `specs/024-infra-hardening/validation.md`.

| Check | Pass condition |
| --- | --- |
| Runbook end to end | Done in 2.4 |
| Access refuses another email | Sign-in with another email is denied |
| `cargo web-pair` over the live route | A browser enrolls; the fingerprints match |
| WEB tab carries a live session | Start, connect, stop, all from the tab |
| Two hosts, live | Switch between machines on the phone; stopping one gateway leaves the other working |
| iPhone background return | Reconnect works after locking the phone |
| Two-hour soak | The session ends at `max_session_seconds`, not before; no gateway errors |
| Cold Worker CPU | Read `/ws` CPU in Workers analytics after each deploy and after idle; no `exceededCpu` |

**Exit:** every row passes, or a failing row has a decision recorded (for
example, Workers Paid for the CPU risk).

## Phase 4: Observability (about 1 week)

### 4.1 Worker logs

- Turn on Workers Logs (`[observability] enabled = true`). Check the Free plan's
  daily limits first.
- `deploymentConfig` currently drops unknown keys, so add `observability` to
  what it passes through.
- Replace the single generic `console.error` with one structured line per
  request: `route`, `host_id`, `outcome`, `duration_ms`. Use these outcomes:
  - `ok`;
  - `auth_missing`, `auth_failed`, `jwks_unavailable`;
  - `policy_rejected`, `unknown_host`;
  - `upstream_refused`, `upstream_timeout`;
  - `internal`.
- Never log the JWT, the email, request headers, or bodies. Add a Worker test
  that asserts a log line never contains the token or the email.

### 4.2 Gateway logs

- Replace `eprintln!` with `tracing`, and write to a daily rolling file under
  `%LOCALAPPDATA%\TopVNC\logs` (or `~/Library/Logs/TopVNC` on macOS), keeping
  7 days. Keep the console output for the WEB tab's row view.
- Keep the current rule: close reasons, byte counts and fingerprints only.
  Never log JWTs, passwords, pixels, input, or clipboard data.

### 4.3 External check and alerts

- Add a scheduled GitHub Action, every 30 minutes, that runs `verifyAccess`
  against production. It expects `/` and `/ws` to answer 302 to the configured
  Access login. On failure it opens or updates a GitHub issue labeled `incident`.
- Limitation: this proves the edge and Access are up. It cannot prove that a
  host is reachable without a credential that bypasses the owner-only policy.
  That gap is accepted, not worked around.
- Turn on Cloudflare notifications for the Worker where the plan offers them.

**Acceptance:** switching off the Access application on staging opens an
incident issue within one check interval.

## Phase 5: Host reliability (1–2 weeks)

### 5.1 Restart policy in the WEB tab

- Restart `cloudflared` and the gateway when they exit unexpectedly, with
  exponential backoff (1, 2, 4, 8 and 16 s). Give up after 5 failures in
  10 minutes and show **Failed** with the last output, as today.
- The UI shows the restart count. Pressing **Stop browser access** never
  triggers a restart.
- Test it with the existing simulated-program harness in `src/web_access/runtime/tests.rs`.

### 5.2 Browser access under the host service (spec first)

Write `specs/025-service-browser-access/spec.md` before writing code. Let the
TopVNC Host service optionally supervise the gateway and `cloudflared` with its
existing restart and backoff, so browser access survives log-off and reboot.
The spec must cover:

- which account owns the token and key files once SYSTEM reads them;
- how pairing works when no one is logged in;
- how the WEB tab shows service-owned processes.

### 5.3 Pin `cloudflared`

- Read `cloudflared --version` at start, in both `topvnc-tunnel` and the WEB tab.
  Refuse anything older than the minimum that supports `--token-file`
  (2026.10.3).
- Warn when the installed version is more than 90 days old. Auto-update stays
  off by design, so document how to update by hand.

### 5.4 Protect secret files on Windows

- `--init-host-key` sets the file's access control list itself: the current
  user and SYSTEM only, with inheritance off. The manual `icacls` step is no
  longer needed.
- `load_host_key` in `crates/topvnc-web-gateway/src/lib.rs:143` checks the
  access list on Windows, as it already checks the mode on Unix. The WEB tab's
  pre-start check in `src/web_access/config.rs` does the same for
  `tunnel-token.txt`. Both refuse a file that other users can read, and print
  the fix.
- Keep the `unsafe` Win32 calls in one small crate, as `topvnc-net` already
  does, so the gateway and app crates stay free of `unsafe`.
- Test: create a file readable by `Users`, and check that start is refused with
  the fix shown.

### 5.5 Browser revocation

- Add `topvnc-web-gateway --list-browsers CONFIG`, which prints each fingerprint
  and its pairing time.
- Add `topvnc-web-gateway --revoke FINGERPRINT CONFIG`, which rewrites
  `browsers.txt` atomically (temporary file, then rename).
- A running gateway reloads `browsers.txt` when it changes, and ends an active
  session whose key was revoked.
- In the WEB tab, list the paired browsers with a **Revoke** button.

**Acceptance:** revoking the connected browser ends its session within
2 seconds, and that browser's next attempt gets "This browser is not enrolled".

## Phase 6: CI completeness (2–3 days)

- **macOS:** add a `Rust (macos-latest)` job that runs on pushes to `main` and
  nightly, not on PRs, so queue delays never block a merge. A failure opens an
  issue. Keep the local Mac check before merging macOS-sensitive PRs.
- **Browser integration:** run `npm run test:integration` after the build in
  the `Browser client and Worker` job. Ubuntu runners include Chrome; set
  `TOPVNC_TEST_BROWSER`.
- **PowerShell helpers:** add a Windows job that runs Pester on
  `scripts/tests/Network.Tests.ps1`.
- **Design wireframes:** run `node --test design/topvnc-mobile-wireframe/tests`,
  or delete the wireframes if they are retired.
- **Spike workspace:** do not add it to CI; it is deleted in 2.4.
- **Python helpers:** keep the `test_cloudflare.py` job, and add the uncommitted
  `run-worker.py` changes on `feat/multi-host-web-control` once they are merged.
- After each new job is stable for a week, add it to the required checks in 0.1.

## Phase 7: Documentation and structure (ongoing, lower priority)

### 7.1 Split the gateway guide

Split `docs/cloudflare-gateway.md` into:

- `docs/browser-access/setup.md`: the one-time setup runbook;
- `docs/browser-access/operations.md`: deploy, rollback, pair, revoke, rotate,
  troubleshooting;
- `docs/browser-access/security-model.md`;
- the "Verified …" and status notes move to the `validation.md` files under
  `specs/`, so the guide states how things work, not their history.

### 7.2 Move server code out of the app crate

Move the host-side protocol and session code out of `src/lib.rs`, with the Tight
encoder from `src/tight.rs`, into a `crates/topvnc-server` crate with no
platform UI dependencies, as `AGENTS.md` requires. Move it in small PRs, each
passing CI with no behavior change. This is not infrastructure-critical; do it
after Phases 0–5.

## Explicitly deferred

These are product limits, not mistakes. Each one needs a spec before it changes:

- one owner email per deployment;
- one browser session per host;
- `workers.dev` origins only (a custom domain needs a paid domain and a
  different Access setup);
- a static `HOSTS_JSON`, so adding a machine means a Worker deploy;
- UDP or H.264 transport (on the roadmap; not infrastructure).

## Schedule

| Phase | Effort | Depends on | Owner |
| --- | --- | --- | --- |
| 0. Quick wins | 1 day | License decision | Dev |
| 1. Release pipeline | 1 week | 0.1, signing decision and credentials | Dev |
| 2. Deployment safety | 1 week | 0.1, deploy-location decision | Dev, plus owner for 2.4 |
| 3. Live validation | 2 days | 2.4 | Owner |
| 4. Observability | 1 week | 2.1 (to test on staging) | Dev |
| 5. Host reliability | 1–2 weeks | 5.2 needs its own spec | Dev |
| 6. CI completeness | 2–3 days | 0.1 | Dev |
| 7. Docs and structure | Ongoing | Phases 0–5 | Dev |

Phases 0, 1 and 6 can run in parallel with Phase 2. The estimates are rough,
for one developer, and do not include waiting time for signing credentials or
Apple notarization setup.

## Definition of done

- No change reaches `main`, a release, or production without passing the
  required checks.
- Every release is built by CI, signed, checksummed and attested.
- A production deploy either passes its post-deploy Access check or rolls itself
  back, and the failure is visible without anyone watching a terminal.
- The validation table in Phase 3 is complete, with dates.
- The host processes recover from crashes on their own, and secret files cannot
  be read by other users.
