import test from "node:test";
import assert from "node:assert/strict";
import { SessionSlot } from "../session-slot.mjs";

test("switch waits for input release and teardown; old messages and diagnostics are fenced", async () => {
  const events = [];
  let late;
  let acknowledge;
  let diagnosticActive;
  const slot = new SessionSlot((message, active) => { events.push(message.type); diagnosticActive = active; });
  await slot.start(async (emit) => {
    late = emit;
    return {
      post(message) {
        if (message.type === "close") {
          events.push("release-held-keys-and-buttons");
          acknowledge = () => emit({ type: "closed" });
        }
      },
      dispose: () => events.push("dispose-old"),
    };
  });
  late({ type: "connected" });
  const stopped = slot.stop();
  const next = slot.start(async (emit) => {
    events.push("start-new");
    emit({ type: "password" });
    return { post() {}, dispose() {} };
  });
  await Promise.resolve();
  assert.equal(diagnosticActive(), false);
  late({ type: "verified" });
  late({ type: "resized" });
  assert.ok(!events.includes("start-new"));
  acknowledge();
  await stopped;
  await next;
  assert.ok(events.indexOf("release-held-keys-and-buttons") < events.indexOf("start-new"));
  assert.ok(events.indexOf("dispose-old") < events.indexOf("start-new"));
  assert.deepEqual(events.filter((v) => ["connected", "password", "verified", "resized", "closed"].includes(v)), ["connected", "password"]);
});

test("switch cancels asynchronous session setup and ignores messages after natural closure", async () => {
  const seen = [];
  const slot = new SessionSlot((message) => seen.push(message.type));
  let finish;
  let stale;
  let disposed = false;
  const starting = slot.start(async (emit, active) => {
    stale = emit;
    await new Promise((resolve) => { finish = resolve; });
    assert.equal(active(), false);
    return { dispose() { disposed = true; }, post() {} };
  });
  await Promise.resolve();
  await slot.stop();
  finish();
  await starting;
  stale({ type: "password" });
  assert.ok(disposed);
  assert.deepEqual(seen, []);
  await slot.start(async (emit) => { stale = emit; return { post() {}, dispose() {} }; });
  stale({ type: "closed" });
  stale({ type: "verified" });
  assert.deepEqual(seen, ["closed"]);
});

test("unresponsive worker is disposed before starting another session", async () => {
  const events = [];
  const slot = new SessionSlot(() => {}, 5);
  await slot.start(async () => ({ post() {}, dispose() { events.push("dispose"); } }));
  slot.stop();
  await slot.start(async () => { events.push("start"); return { post() {}, dispose() {} }; });
  assert.deepEqual(events, ["dispose", "start"]);
});
