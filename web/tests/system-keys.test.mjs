import test from "node:test";
import assert from "node:assert/strict";
import { SYSTEM_KEYS, lockSystemKeys } from "../system-keys.mjs";

function fakeKeyboard(lock = () => Promise.resolve()) {
  const calls = [];
  return {
    calls,
    lock(keys) { calls.push(["lock", keys]); return lock(keys); },
    unlock() { calls.push(["unlock"]); },
  };
}

test("the Windows keys are locked only in fullscreen with the setting on", () => {
  const keyboard = fakeKeyboard();
  assert.equal(lockSystemKeys(keyboard, { fullscreen: true, enabled: true }), true);
  assert.equal(lockSystemKeys(keyboard, { fullscreen: false, enabled: true }), false);
  assert.equal(lockSystemKeys(keyboard, { fullscreen: true, enabled: false }), false);
  assert.deepEqual(keyboard.calls, [["lock", ["MetaLeft", "MetaRight"]], ["unlock"], ["unlock"]]);
  assert.deepEqual(SYSTEM_KEYS, ["MetaLeft", "MetaRight"]);
});

test("browsers without Keyboard Lock leave the keys alone", () => {
  assert.equal(lockSystemKeys(undefined, { fullscreen: true, enabled: true }), false);
  assert.equal(lockSystemKeys({}, { fullscreen: true, enabled: true }), false);
  assert.equal(lockSystemKeys({ lock() {} }, { fullscreen: true, enabled: true }), false);
});

test("a refused lock is not an error", async () => {
  let rejected = 0;
  const refused = fakeKeyboard(() => { rejected++; return Promise.reject(new DOMException("refused", "InvalidStateError")); });
  assert.equal(lockSystemKeys(refused, { fullscreen: true, enabled: true }), true);
  const throwing = fakeKeyboard(() => { throw new TypeError("not allowed"); });
  assert.equal(lockSystemKeys(throwing, { fullscreen: true, enabled: true }), false);
  // Let the rejection settle; an unhandled one would fail the test run.
  await new Promise((resolve) => setTimeout(resolve, 0));
  assert.equal(rejected, 1);
});
