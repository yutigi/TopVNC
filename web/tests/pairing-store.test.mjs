import test from "node:test";
import assert from "node:assert/strict";
import { forgetPairing, inviteFromHash, isKey, loadPairing, savePairing } from "../pairing-store.mjs";

class MemoryStorage {
  #items = new Map();
  getItem(key) { return this.#items.has(key) ? this.#items.get(key) : null; }
  setItem(key, value) { this.#items.set(key, String(value)); }
  removeItem(key) { this.#items.delete(key); }
}

const host = "A".repeat(43);
const browser = "b_-".repeat(14) + "c";

test("stores, reloads and forgets one pairing", () => {
  const storage = new MemoryStorage();
  assert.equal(loadPairing(storage), undefined);
  savePairing(storage, { host, browser, secret: "never stored" });
  assert.deepEqual(loadPairing(storage), { host, browser });
  assert.doesNotMatch(storage.getItem("topvnc.e2e.pairing.v1"), /secret/);
  forgetPairing(storage);
  assert.equal(loadPairing(storage), undefined);
});

test("treats corrupt, altered or unavailable storage as not paired", () => {
  const storage = new MemoryStorage();
  for (const raw of ["{", "null", JSON.stringify({ version: 2, host, browser }),
    JSON.stringify({ version: 1, host: host.slice(1), browser }),
    JSON.stringify({ version: 1, host, browser: `${browser}=` }), "x".repeat(600)]) {
    storage.setItem("topvnc.e2e.pairing.v1", raw);
    assert.equal(loadPairing(storage), undefined, raw);
  }
  const blocked = { getItem() { throw new Error("SecurityError"); }, removeItem() { throw new Error("SecurityError"); } };
  assert.equal(loadPairing(blocked), undefined);
  assert.equal(loadPairing(undefined), undefined);
  assert.doesNotThrow(() => forgetPairing(blocked));
  assert.throws(() => savePairing(undefined, { host, browser }));
  assert.throws(() => savePairing(storage, { host: "short", browser }));
});

test("reads only bounded #pair= fragments", () => {
  assert.equal(inviteFromHash("#pair=v1.abc.def"), "v1.abc.def");
  for (const hash of ["", "#", "#pair=", "#other=v1", "?pair=v1", `#pair=${"x".repeat(129)}`, undefined]) {
    assert.equal(inviteFromHash(hash), undefined, String(hash));
  }
  assert.equal(isKey(host), true);
  assert.equal(isKey(`${host}A`), false);
});
