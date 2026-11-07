import test from "node:test";
import assert from "node:assert/strict";
import { forgetPairing, inviteFromHash, isKey, loadPairing, savePairing, migratePairing } from "../pairing-store.mjs";

class MemoryStorage {
  #items = new Map();
  getItem(key) { return this.#items.has(key) ? this.#items.get(key) : null; }
  setItem(key, value) { this.#items.set(key, String(value)); }
  removeItem(key) { this.#items.delete(key); }
}

const host = "A".repeat(43);
const browser = "b_-".repeat(14) + "c";

test("machine pairings never read, overwrite, or forget another machine's keys", () => {
  const storage = new MemoryStorage();
  savePairing(storage, { host, browser });
  assert.equal(loadPairing(storage, "gaming-pc"), undefined);
  savePairing(storage, { host, browser }, "gaming-pc");
  const office = { host: "C".repeat(43), browser: "D".repeat(43) };
  savePairing(storage, office, "office-pc");
  forgetPairing(storage, "gaming-pc");
  assert.deepEqual(loadPairing(storage, "office-pc"), office);
  assert.deepEqual(loadPairing(storage), { host, browser });
  assert.throws(() => savePairing(storage, office, "../bad"));
});

test("legacy migration requires an explicit target and preserves existing pins or failed writes", () => {
  const storage = new MemoryStorage();
  savePairing(storage, { host, browser });
  migratePairing(storage, null);
  assert.deepEqual(loadPairing(storage), { host, browser });
  const different = { host: "C".repeat(43), browser };
  savePairing(storage, different, "office-pc");
  migratePairing(storage, "office-pc");
  assert.deepEqual(loadPairing(storage, "office-pc"), different);
  assert.ok(loadPairing(storage));
  assert.throws(() => migratePairing({ getItem: storage.getItem.bind(storage), setItem() { throw Error("blocked"); } }, "gaming-pc"));
  assert.ok(loadPairing(storage));
  migratePairing(storage, "gaming-pc");
  assert.equal(loadPairing(storage), undefined);
  assert.deepEqual(loadPairing(storage, "gaming-pc"), { host, browser });
  assert.equal(loadPairing(storage, "other"), undefined);
});

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
