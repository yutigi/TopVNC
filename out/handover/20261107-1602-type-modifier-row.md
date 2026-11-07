# Session handover: Type modifiers and keyboard language icon

Created 2026-11-07 16:02 Asia/Bangkok (UTC+07:00).
Repo/worktree: `D:/FreetimeProject/AI/TopVNC/.worktrees/mobile-controls-wireframe`.
Branch: `design/mobile-controls-wireframe`; clean at `779f973` before this briefing.

## Goal

Implement the user's latest request in the existing Sites wireframe:
**In Type, replace the pinned language row with Ctrl, Alt, and Shift. Move
language switching into an icon inside the on-screen keyboard.**
Then validate, publish the updated Site for the user to verify, and commit.
This requested UI change has NOT been implemented yet; it is the handover focus.

## Where things stand

- TopVNC's real Rust/native/web clients were not changed by this design work.
- Prototype source: `design/topvnc-mobile-wireframe/` in this worktree.
- Feature scope/acceptance: `specs/021-mobile-controls-wireframe/spec.md`.
- Live private Site: https://topvnc-mobile-wireframe-nov26.js-sookudom-temp.chatgpt.site
- Direct FPS preview: same URL with `?preset=fps`.
- Site project ID: `appgprj_6aeedaa6ea0c81918528d7a80229d4d3`, also recorded in
  `design/topvnc-mobile-wireframe/.openai/hosting.json`.
- Recent commits:
  - `779f973` design(web): add FPS preset and keep languages in Type
  - `1b2585b` design(web): add custom keys and layout controls
  - `0e14357` design(web): add mobile keyboard wireframe
- Existing prototype: custom multilingual keyboards, optional native OS input,
  configurable custom keys and UI layouts, independent portrait/landscape views,
  FPS preset with simulated touch holds, aiming, remapping and handedness.
- English, Thai, Chinese and Japanese are editable examples. Users can add their
  own layouts. Chinese/Japanese conversion uses labeled demo fixtures, not a full IME.
- Current language pins appear only in Type; already absent from Quick Keys and FPS.
- Current keyboard already has a language icon, but the separate language row remains.
- Last checks passed: 11 focused Node tests, JS syntax/browser module bundling,
  CSS parsing, HTML references, Cargo formatting, workspace tests and Clippy.
- Publication succeeded for the latest FPS version. Source commit in the separate
  Sites repository: `cfd608c1e29c86112e0db0e3769693be1a9e966e`.
- Only this handover file is new in this turn. No local server is running.

## Decisions and constraints

- User explicitly requested a wireframe in Sites first, before production integration.
- User: **Do not serve locally. Publish to Sites and let the user verify.**
- User: **Work in a separate worktree and always commit completed work.**
- Preserve unrelated multi-host changes in the original checkout at
  `D:/FreetimeProject/AI/TopVNC`; they are not part of this branch or task.
- Stable thumb positions and muscle memory in both orientations are primary goals.
- Preserve language/layout choices, custom keys, native input and FPS functionality.
- Keep simulated state explicit; do not claim game, platform, IME or latency support.
- This task is authorized; no additional conversational approval is needed to edit,
  commit, or republish this same owner-private Site. Keep its audience unchanged.

## Next steps

1. **Implement the requested Type modifier row and keyboard-only language switch.**
   Start at `dist/index.html:53` (language bar), `:58` (existing Quick Keys modifiers),
   and `:61` (Type keyboard and existing `#keyboard-languages` icon).
2. Reuse the language picker flow at `dist/app.mjs:377` and `buildLanguages()`.
   Let the keyboard icon select/configure languages without showing pinned chips
   above Type. Preserve saved profiles and editable language layouts.
3. Implement visible Ctrl/Alt/Shift states and useful custom-keyboard chord behavior.
   Relevant code: `dist/app.mjs:48` release, `:133` modifier click handler,
   `addCharacter()`, `renderKeyboard()`, and `pressKey()`. Currently custom character
   insertion ignores the modifier Set: simply adding buttons would be incomplete.
   Keep Shift key labels synchronized; release modifiers after use, mode changes,
   rotation, cancellation and focus loss. Preserve IME composition guards.
4. Update customization group names/settings/sketch at `dist/customize.mjs:70`
   and `:190`, plus `keyboard.css`, `customize.css`, README and feature spec.
   Do not accidentally let a legacy hide-language preference hide required modifiers.
   Keep the replacement row Type-only; FPS Play should remain uncluttered.
5. Validate focused behavior, then run the project-required gates. Focused tests:
   `node --test tests/customize.test.mjs tests/fps.test.mjs` from the prototype folder.
   Workspace gates: `cargo fmt --all -- --check`, `cargo test --workspace`,
   `cargo clippy --workspace --all-targets`. Reusing the original repo's `target/`
   with `--target-dir D:/FreetimeProject/AI/TopVNC/target` avoids a full rebuild.
6. Follow the Sites skill and republish the same Site. The ignored publication
   checkout is `design/topvnc-mobile-wireframe/.sites-runtime/checkout`.
   Read/get the Site, renew credentials, open with the source helper, copy changed
   tracked source into that checkout, package, and privately deploy the exact SHA.
7. Commit only intended files on this branch, verify clean status, and give the user
   the updated Sites link, short change summary, checks and new commit hash.

## Publishing notes / blockers

- Sites skill: `C:/Users/USER/.codex/plugins/cache/openai-curated-remote/sites/1.1.4/skills/sites/SKILL.md`.
- Helpers: sibling plugin `scripts/site-workflow.mjs`; static output is `dist`.
- Pass credentials only through helper stdin; never store/print tokens.
- Network helper needed `require_escalated`; sandbox Git network access stalled.
- Escalated user differs from the sandbox Git owner. Use a **process-only**
  `GIT_CONFIG_COUNT=1`, key `safe.directory`, value equal to the exact publication
  checkout path above. Do not modify global Git configuration.
- PTY output wraps JSON with ANSI sequences. After successful packaging, read
  `git rev-parse HEAD` in that checkout for the full exact pushed commit SHA.
- Browser QA unavailable: browser host failed, and native Windows fallback stopped
  because it could not verify the active URL. Do not bypass that restriction.
  The user asked to perform visual verification on the published Site themselves.
- Handover helper detected Warp. Codex was not found by `Get-Command` in the
  sandbox environment (combined detection command exit 1); if launching is requested,
  check CLI availability in the normal user environment before claiming success.

## Guard rails

Read applicable `AGENTS.md`, any `CLAUDE.md`, and the project constitution if present.
Preserve existing work, authentication/security boundaries, and all current user
preferences. Do not merge, push this application branch, or modify the real client
unless separately requested. Publishing this design Site is already in scope.
