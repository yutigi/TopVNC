import test from "node:test";
import assert from "node:assert/strict";
import { validateHosts, legacyHosts, fetchHosts, inviteTarget, invitePolicy, socketPath } from "../hosts.mjs";

const registry = { legacy: false, legacyHostId: "gaming-pc", hosts: [
  { id: "gaming-pc", name: "Gaming PC" }, { id: "office-pc", name: "Office PC" },
] };

test("allowlisted machine paths and explicitly targeted invites", () => {
  validateHosts(registry);
  assert.equal(socketPath(registry, "office-pc"), "/ws/office-pc");
  assert.equal(inviteTarget(registry, "?host=office-pc"), "office-pc");
  for (const query of ["", "?host=unknown", "?host=../ws", "?host=gaming-pc&host=office-pc"]) {
    assert.throws(() => inviteTarget(registry, query));
  }
  assert.throws(() => socketPath(registry, "unknown"));
  assert.equal(inviteTarget(legacyHosts(), ""), "default");
  assert.equal(socketPath(legacyHosts(), "default"), "/ws");
});

test("changed host pins are rejected until the specific pairing is forgotten", () => {
  assert.equal(invitePolicy(undefined, "key-a"), "accept");
  assert.equal(invitePolicy({ host: "key-a" }, "key-a"), "already-paired");
  assert.equal(invitePolicy({ host: "key-a" }, "key-b"), "changed-key");
});

test("malformed and ambiguous public registries fail closed", () => {
  for (const value of [null, {}, { ...registry, hosts: [] }, { ...registry, hosts: Array(33).fill(registry.hosts[0]) },
    { ...registry, legacyHostId: "missing" }, { ...registry, legacy: true },
    { ...registry, hosts: [registry.hosts[0], registry.hosts[0]] },
    { ...registry, hosts: [{ id: "bad/id", name: "Bad" }] }]) assert.throws(() => validateHosts(value));
});

test("metadata failures never downgrade production; oversized streaming responses are cancelled", async () => {
  for (const status of [401, 403, 404, 503]) await assert.rejects(fetchHosts(async () => new Response("", { status })));
  assert.deepEqual(await fetchHosts(async () => new Response("", { status: 404 }), true), legacyHosts());
  assert.deepEqual(await fetchHosts(async () => new Response(JSON.stringify(registry))), registry);
  let cancelled = false;
  const stream = new ReadableStream({
    pull(controller) { controller.enqueue(new Uint8Array(8193)); },
    cancel() { cancelled = true; },
  });
  await assert.rejects(fetchHosts(async () => new Response(stream)));
  assert.equal(cancelled, true);
});
