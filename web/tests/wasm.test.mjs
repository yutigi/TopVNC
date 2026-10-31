// Smoke test of the built bindings (run `npm run build` first): the API the
// page and Worker use, through the real WebAssembly module.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import init, { Connection, fingerprint, newBrowserKey, parseInvite, publicKey } from "../build/pkg/topvnc_web.js";

await init({ module_or_path: readFileSync(new URL("../build/pkg/topvnc_web_bg.wasm", import.meta.url)) });

test("keys, fingerprints, and invites cross the boundary as base64url text", () => {
  const browser = newBrowserKey();
  assert.match(browser, /^[A-Za-z0-9_-]{43}$/);
  const pub = publicKey(browser);
  assert.notEqual(pub, browser);
  assert.match(fingerprint(pub), /^[0-9a-f]{4}(-[0-9a-f]{4}){3}$/);
  const invite = parseInvite(`v1.${pub}.${browser}`);
  assert.equal(invite.host, pub);
  assert.equal(invite.secret, browser);
  invite.free();
  assert.throws(() => parseInvite("v1.short"));
  assert.throws(() => fingerprint("bad"));
});

test("a connection starts with the fixed-size first handshake message and refuses garbage", () => {
  const host = publicKey(newBrowserKey());
  const connection = new Connection(newBrowserKey(), host, undefined, 6);
  const first = connection.takeOutgoing();
  assert.equal(first.length, 50);
  assert.equal(connection.takeOutgoing(), undefined);
  assert.equal(connection.width(), 0);
  assert.throws(() => connection.receive(new Uint8Array(48), 0));
  // Failed for good: nothing more is accepted.
  assert.throws(() => connection.receive(new Uint8Array(48), 0));
  connection.free();
  assert.throws(() => new Connection("bad", host, undefined, 6));
});
