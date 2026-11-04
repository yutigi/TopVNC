# Tasks

## Phase 1: Run view (US1, US2)

- [x] T001 Gateway `--supervised` mode: stdin `pair`/`cancel-pair`/EOF, stdout protocol, no QR or link on stderr; `Gateway::cancel_pairing`; integration test with the real binary.
- [x] T002 `src/web_access` paths and config checks, with tests for placeholders, non-loopback addresses, a bad session cap, and missing files.
- [x] T003 Line rules and redaction, with tests for gateway and cloudflared lines.
- [x] T004 Supervised process with bounded output, no console window, and the Windows Job Object.
- [x] T005 `WebRuntime` state machine with a fake spawner: start order, time limits, child exit, stop order, server reuse rules, pairing lifecycle.
- [x] T006 QR matrix from a `pair` line; the link is not kept.
- [x] T007 WEB tab: layout, clicks, Enter, wiring in `main.rs`; `HostedServer` records plain RFB.
- [x] T008 Docs (`docs/cloudflare-gateway.md`) and `validation.md`; fmt, test, clippy.
- [ ] T009 Owner live check: Start, phone session, Pair a browser, Stop, close TopVNC, kill TopVNC; no leftover processes (SC-001, SC-002).

## Phase 2: Setup view (US3)

- [ ] T010 Update plan.md with the Setup design and get it reviewed.
- [ ] T011 to T018 One task per Setup step, defined in the T010 update.
