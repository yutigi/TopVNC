# Session handover: Implement the mobile UI in the real web client

Created 2026-11-07 17:04 Asia/Bangkok (UTC+07:00).
Repo/worktree: `D:/FreetimeProject/AI/TopVNC/.worktrees/mobile-controls-wireframe`.
Branch: `design/mobile-controls-wireframe`; clean at `8177f26` before this briefing.

## Goal

The user now explicitly requests **implementation of the new UI design in the
real web client**, and requested this new visible terminal/Codex CLI session to
start that work. This extends the earlier wireframe-only scope. Continue actual
implementation, validation and commits; do not stop at another design proposal.

Use the current mobile wireframe as the design reference. Done means its mobile
controls and interactions are integrated with real web session/input behavior,
the existing client behavior remains usable, meaningful tests and required gates
pass, documentation reflects implemented behavior, and completed work is committed.
Production hosting/infrastructure deployment is a separate action, not implied.

## Where things stand

- The Rust client, gateway and real `web/` client have NOT been modified by the
  wireframe work. All design implementation is in `design/topvnc-mobile-wireframe/`.
- Latest private reference Site:
  https://topvnc-mobile-wireframe-nov26.js-sookudom-temp.chatgpt.site
- FPS reference: same URL with `?preset=fps`.
- Design acceptance criteria: `specs/021-mobile-controls-wireframe/spec.md`.
- Reference instructions/limitations: `design/topvnc-mobile-wireframe/README.md`.
- Recent commits:
  - `8177f26` design(web): add hold-and-slide keyboard language switching
  - `e3873a5` design(web): add Type modifier row and keyboard language picker
  - `779f973` design(web): add FPS preset and keep languages in Type
  - `1b2585b` design(web): add custom keys and layout controls
  - `0e14357` design(web): add mobile keyboard wireframe
- Latest Site source SHA: `786455a98523300958bec4111ed245b5ab0d845f`.
  Its private deployment succeeded. This SHA belongs to the separate Sites repo.
- No uncommitted application changes before writing this handover. No local
  preview server is running. No active `.service-ops/active-record` was present.

## Design to implement

- Stable Keys / Type / Pointer / Enter thumb dock; portrait and landscape layouts
  retain familiar control order and reach. Preserve a useful remote display area.
- Type defaults to a custom on-screen keyboard, with optional native OS input.
  Type-only Ctrl / Alt / Shift row replaces separate language chips. Shift state
  and keyboard labels stay synchronized; modifiers apply to the next key/chord.
- Globe key: tap cycles profiles; hold 350 ms opens an anchored language menu;
  slide highlights a language, release chooses it. Release on globe leaves the
  menu open for tapping/scrolling; release outside cancels. Current language is
  checked. Manage languages opens profile/layout configuration.
- Globe menu includes keyboard/assistive access and cancels on mode/source or
  orientation changes, focus loss, hidden page, cancellation and outside press.
- English, Thai, Chinese and Japanese are editable examples; users can add other
  layouts. Browser-local preferences include up to four favorites, custom rows,
  custom keys/snippets, and separate portrait/landscape UI arrangements.
- Customization configures keys, group/dock ordering, deck placement and sizing.
  Legacy hide-language preferences must never hide required modifier controls.
- FPS preset has simultaneous movement/action holds, aiming, remapping and
  handedness, separate from Type. Release held inputs safely on all exit paths.
- Wireframe Chinese/Japanese candidate conversion is explicitly a tiny DEMO
  fixture. Do not ship those samples as a claimed complete IME. Preserve native
  composition/Unicode handling and be explicit about custom conversion limits.
- Wireframe feedback is simulated. Real integration must route through the
  existing session APIs, not copy simulated output into production behavior.

## Next steps

1. **Start implementing the design in `web/`.** First read applicable guidance,
   the reference spec/README, and inspect the current real client. Record the
   production scope/acceptance in a new `specs/` feature before this large change;
   retain feature 021 as the wireframe record. Use initialized Spec Kit conventions.
2. Trace real input/session boundaries before porting UI:
   - `web/index.html:60`: current scrolling toolbar and session controls.
   - `web/app.mjs:272`: viewer layout; `:349`: pointer; `:495`: keyboard;
     `:529`: native text attachment; `:548`: toolbar/menu; `:687`: lifecycle.
   - `web/text-input.mjs`: native beforeinput/input/composition deduplication.
   - `web/session-input.mjs`: transport backpressure/coalescing and releases.
   - `web/gestures.mjs`, `web/view.mjs`, `web/session.mjs`, `web/session-worker.mjs`.
   Preserve pairing, sign-in, reconnect, display/served-size/quality controls,
   fullscreen/fit/zoom, pointer behavior and physical keyboard input.
3. Reuse/adapt reference modules rather than embedding a second session model:
   `design/topvnc-mobile-wireframe/dist/{app,customize,fps,language-switcher}.mjs`
   and its CSS/HTML. Keep UI distinct from reusable protocol/session/input logic.
   Port only intentional product UI, not the surrounding design-lab inspector.
4. Add focused integration tests for real key down/up ordering, chord consumption,
   text/composition deduplication, language gestures, safe lifecycle releases,
   custom preference validation, layout migration and independent FPS touches.
   Existing real tests are under `web/tests/`; inspect `web/package.json` and
   `web/scripts/build.mjs` for build/WASM prerequisites and commands.
5. Validate with `npm test` in `web/`, its build and relevant WASM checks when
   applicable. Required repository gates: `cargo fmt --all -- --check`,
   `cargo test --workspace`, `cargo clippy --workspace --all-targets`.
   The existing cache can be reused with
   `--target-dir D:/FreetimeProject/AI/TopVNC/target` (needs normal-user access).
6. Review diffs, commit intended completed changes on this isolated worktree,
   verify status, and report behavior/checks and device-verification limits.

## Decisions, constraints and environment

- User: work in a separate worktree and always commit completed work. This is
  already the dedicated worktree. Preserve unrelated multi-host work in the
  original checkout `D:/FreetimeProject/AI/TopVNC`; do not switch/modify it.
- User's earlier instruction was: do not serve the wireframe locally; publish
  to Sites and let them verify. No local preview has been started since then.
  Keep that preference; tests/builds can proceed. Do not deploy the real client
  or change production infrastructure merely because Site edits were authorized.
- Previous checks passed: all 27 focused wireframe Node tests; browser module
  bundling; CSS parsing; HTML references; Cargo format, workspace tests and Clippy.
- On this Windows host, workspace tests initially failed (exit 1) in unchanged
  `settings::tests::password_is_protected_for_current_user` due console mojibake.
  `chcp.com 65001 > $null` before Cargo made the FULL suite pass (exit 0).
- Shared target writes and worktree Git index writes need normal-user/elevated
  tool execution because they live outside sandbox writable roots. Ordinary
  sandbox attempts failed with access denied (exit 1 / Git fatal index.lock).
- Browser QA was unavailable in the earlier design session. The browser host
  failed and native fallback could not verify the active URL; do not bypass it.
  No real-device touch/IME or latency/support claims have been validated.
- The tracked design `.openai/hosting.json` contains only Site ID + `dist` path,
  no credentials; the user asked and was told its current contents are safe to
  publish. `.sites-runtime/` remains ignored. Never commit credentials/runtime data.
- Handover launcher detected Warp. Codex is available in normal-user Windows at
  `C:/Users/USER/AppData/Local/Programs/OpenAI/Codex/bin/codex.exe`; the sandbox
  PATH alone did not find it. No CLI model/sandbox/approval overrides were requested.

## Guard rails

Read applicable `AGENTS.md`, `CLAUDE.md` if present, and project constitution if
present. The current `.specify/memory/constitution.md` is still a placeholder
template, not adopted extra principles. Follow `AGENTS.md` security, platform
boundaries, bounded input/state and validation requirements. Preserve existing
work. Do not merge, push the application branch, or deploy production unless
separately instructed. This user's new request DOES authorize local production
web-client UI implementation and commits; no repeat conversational approval needed.
