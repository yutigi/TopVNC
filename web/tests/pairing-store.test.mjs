import test from "node:test";
import assert from "node:assert/strict";
import { forgetPairing, inviteFromHash, isKey, loadPairing, savePairing, migratePairing, strandedLegacyPairing } from "../pairing-store.mjs";

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
  const legacy = { host: "E".repeat(43), browser: "F".repeat(43) };
  savePairing(storage, legacy);
  assert.equal(loadPairing(storage, "gaming-pc"), undefined);
  savePairing(storage, { host, browser }, "gaming-pc");
  const office = { host: "C".repeat(43), browser: "D".repeat(43) };
  savePairing(storage, office, "office-pc");
  forgetPairing(storage, "gaming-pc");
  assert.equal(loadPairing(storage, "gaming-pc"), undefined);
  assert.deepEqual(loadPairing(storage, "office-pc"), office);
  assert.deepEqual(loadPairing(storage), legacy, "a different single-host pairing is not a copy");
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
  assert.deepEqual(loadPairing(storage, "gaming-pc"), { host, browser });
  assert.equal(loadPairing(storage, "other"), undefined);
});

test("migration copies the v1 pairing so a single-host rollback still finds it", () => {
  const storage = new MemoryStorage();
  savePairing(storage, { host, browser });
  migratePairing(storage, "gaming-pc");
  migratePairing(storage, "gaming-pc"); // Every load repeats it; nothing changes.
  assert.deepEqual(loadPairing(storage), { host, browser });
  assert.deepEqual(loadPairing(storage, "gaming-pc"), { host, browser });
  // Forgetting the machine removes both copies, so the next load cannot restore it.
  forgetPairing(storage, "gaming-pc");
  migratePairing(storage, "gaming-pc");
  assert.equal(loadPairing(storage), undefined);
  assert.equal(loadPairing(storage, "gaming-pc"), undefined);
});

test("forgetting the migration target stays forgotten when v1 holds a different pairing", () => {
  const storage = new MemoryStorage();
  const old = { host: "E".repeat(43), browser: "F".repeat(43) };
  savePairing(storage, old);
  savePairing(storage, { host, browser }, "gaming-pc");
  assert.equal(strandedLegacyPairing(storage, "gaming-pc"), true);
  // Another machine is not the migration target: its forget leaves v1 alone.
  savePairing(storage, { host: "C".repeat(43), browser: "D".repeat(43) }, "office-pc");
  forgetPairing(storage, "office-pc", "gaming-pc");
  assert.deepEqual(loadPairing(storage), old);
  forgetPairing(storage, "gaming-pc", "gaming-pc");
  migratePairing(storage, "gaming-pc"); // The next load must not restore the old pin.
  assert.equal(loadPairing(storage, "gaming-pc"), undefined);
  assert.equal(loadPairing(storage), undefined);
  assert.equal(strandedLegacyPairing(storage, "gaming-pc"), false);
});

test("a v1 pairing no machine uses is reported as stranded until forgotten", () => {
  const storage = new MemoryStorage();
  assert.equal(strandedLegacyPairing(storage, null), false);
  savePairing(storage, { host, browser });
  assert.equal(strandedLegacyPairing(storage, null), true, "no migration target");
  assert.equal(strandedLegacyPairing(storage, "gaming-pc"), false, "the next load migrates into an empty slot");
  migratePairing(storage, "gaming-pc");
  assert.equal(strandedLegacyPairing(storage, "gaming-pc"), false, "a kept copy is not stranded");
  savePairing(storage, { host: "C".repeat(43), browser: "D".repeat(43) }, "office-pc");
  assert.equal(strandedLegacyPairing(storage, "office-pc"), true, "the target holds a different pin");
  forgetPairing(storage);
  assert.equal(strandedLegacyPairing(storage, null), false);
  assert.deepEqual(loadPairing(storage, "gaming-pc"), { host, browser }, "forgetting v1 keeps scoped pairings");
  assert.equal(strandedLegacyPairing(undefined, null), false);
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
