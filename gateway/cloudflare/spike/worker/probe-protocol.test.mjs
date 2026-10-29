import test from "node:test";
import assert from "node:assert/strict";
import { makePayload, verifyChunk } from "./probe-protocol.mjs";

test("verifies an echo split across arbitrary transport chunks", () => {
  const payload = makePayload(3);
  let offset = 0;
  for (const boundary of [1, 17, 19, 512, 1024]) {
    offset = verifyChunk(payload, offset, payload.slice(offset, boundary));
  }
  assert.equal(offset, payload.length);
});

test("rejects altered, excess, empty, stale and unsolicited echo data", () => {
  const payload = makePayload(4);
  const altered = payload.slice();
  altered[900] ^= 1;
  assert.throws(() => verifyChunk(payload, 0, altered));
  assert.throws(() => verifyChunk(payload, 0, makePayload(3)));
  assert.throws(() => verifyChunk(payload, 0, makePayload(4 + 251)));
  assert.throws(() => verifyChunk(payload, 1, payload));
  assert.throws(() => verifyChunk(payload, 0, new Uint8Array()));
  assert.throws(() => verifyChunk(undefined, 0, payload));
  assert.throws(() => verifyChunk(payload, -1, payload));
  assert.throws(() => verifyChunk(payload, payload.length, payload));
  assert.throws(() => makePayload(-1));
});
