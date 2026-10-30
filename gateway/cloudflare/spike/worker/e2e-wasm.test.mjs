// Smoke test of the built WASM bindings (run `npm run build` first). Protocol
// interoperability with the host is covered by Rust tests against the same crate.
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { initSync, E2eSession, fingerprint, newBrowserKey, parseInvite, publicKey }
  from "./build/browser-pkg/topvnc_spike_browser.js";

initSync({ module: readFileSync(new URL("./build/browser-pkg/topvnc_spike_browser_bg.wasm", import.meta.url)) });

test("generates keys and fingerprints through the bindings", () => {
  const browser = newBrowserKey();
  assert.match(browser, /^[A-Za-z0-9_-]{43}$/);
  assert.notEqual(newBrowserKey(), browser);
  const pub = publicKey(browser);
  assert.match(pub, /^[A-Za-z0-9_-]{43}$/);
  assert.match(fingerprint(pub), /^[0-9a-f]{4}(-[0-9a-f]{4}){3}$/);
  assert.throws(() => publicKey("short"));
});

test("starts session and pairing handshakes with bound headers", () => {
  const host = publicKey(newBrowserKey());
  const secret = publicKey(newBrowserKey());
  const invite = parseInvite(`v1.${host}.${secret}`);
  assert.equal(invite.host, host);
  assert.equal(invite.secret, secret);
  invite.free();
  for (const [pairing, mode] of [[undefined, 1], [secret, 2]]) {
    const session = new E2eSession(newBrowserKey(), host, pairing);
    const first = session.firstMessage();
    assert.ok(first instanceof Uint8Array);
    assert.equal(first.length, 50);
    assert.deepEqual([...first.slice(0, 2)], [1, mode]);
    session.free();
  }
  assert.throws(() => parseInvite(`v2.${host}.${secret}`));
});

test("fails closed on a forged host reply and stays failed", () => {
  const session = new E2eSession(newBrowserKey(), publicKey(newBrowserKey()), undefined);
  assert.throws(() => session.seal(new Uint8Array([1])));
  assert.throws(() => session.finish(new Uint8Array(48)));
  assert.throws(() => session.finish(new Uint8Array(48)));
  assert.throws(() => session.open(new Uint8Array(32)));
  session.free();
});
