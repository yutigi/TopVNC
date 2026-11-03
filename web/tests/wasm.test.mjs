// Smoke test of the built bindings (run `npm run build` first): the API the
// page and Worker use, through the real WebAssembly module.
import test from "node:test";
import { createSession } from "../session.mjs";
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

// Exercise the real session timer and WASM queue while an upgrade is delayed.
for (const delay of [0, 999, 1001, 5000]) {
  test(`the first Noise message survives a ${delay} ms WebSocket upgrade`, async (t) => {
    const sockets = [];
    class DelayedSocket {
      static OPEN = 1;
      readyState = 0;
      protocol = "binary";
      bufferedAmount = 0;
      sent = [];
      constructor() { sockets.push(this); }
      send(message) { this.sent.push(new Uint8Array(message)); }
      close() { this.readyState = 3; }
      open() {
        this.readyState = DelayedSocket.OPEN;
        this.onopen();
      }
    }
    const originalSocket = globalThis.WebSocket;
    globalThis.WebSocket = DelayedSocket;
    t.after(() => { globalThis.WebSocket = originalSocket; });
    t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
    const events = [];
    const message = createSession((event) => events.push(event));
    t.after(() => message({ type: "close" }));
    await message({
      type: "connect", url: "wss://example.invalid/ws",
      browser: newBrowserKey(), host: publicKey(newBrowserKey()), quality: 6,
    });
    const socket = sockets[0];
    t.mock.timers.tick(delay);
    assert.equal(socket.sent.length, 0);
    assert.equal(events.some((event) => event.type === "closed"), false);
    socket.open();
    assert.equal(socket.sent.length, 1, "send the queued handshake on open");
    assert.equal(socket.sent[0].length, 50);
    t.mock.timers.tick(1000);
    assert.equal(socket.sent.length, 1, "statistics must not duplicate the handshake");
  });
}

function fakeSockets(t, { refuse } = {}) {
  const sockets = [];
  class FakeSocket {
    static OPEN = 1;
    readyState = 0;
    protocol = "binary";
    bufferedAmount = 0;
    sent = [];
    closed = false;
    constructor() {
      if (refuse?.()) throw new SyntaxError("The WebSocket URL is invalid.");
      sockets.push(this);
    }
    send(message) { this.sent.push(new Uint8Array(message)); }
    close() { this.readyState = 3; this.closed = true; }
  }
  const originalSocket = globalThis.WebSocket;
  globalThis.WebSocket = FakeSocket;
  t.after(() => { globalThis.WebSocket = originalSocket; });
  return sockets;
}

const request = (extra) => ({
  type: "connect", url: "wss://example.invalid/ws",
  browser: newBrowserKey(), host: publicKey(newBrowserKey()), quality: 6, ...extra,
});

test("each startup failure reaches the page, releases its resources, and allows a retry", async (t) => {
  let refuse = false;
  const sockets = fakeSockets(t, { refuse: () => refuse });
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const freed = t.mock.method(Connection.prototype, "free");
  const events = [];
  const message = createSession((event) => events.push(event));
  t.after(() => message({ type: "close" }));
  const closed = () => events.filter((event) => event.type === "closed");

  // The WebAssembly connection rejects its keys before any socket exists.
  await message(request({ browser: "bad" }));
  assert.equal(sockets.length, 0);
  // The WebSocket constructor throws: the new connection is freed.
  refuse = true;
  await message(request());
  refuse = false;
  assert.equal(freed.mock.callCount(), 1);
  // Drawing setup fails after the socket exists: both are released.
  await message(request({ canvas: { getContext() { throw new Error("No 2D context."); } } }));
  assert.equal(sockets.length, 1);
  assert.equal(sockets[0].closed, true);
  assert.equal(freed.mock.callCount(), 2);

  assert.deepEqual(
    closed().map(({ code, reason, connected }) => [code, reason, connected]),
    Array(3).fill([4007, "The viewer failed to start.", false]),
  );
  assert.match(closed()[1].detail, /WebSocket URL is invalid/);
  assert.match(closed()[2].detail, /No 2D context/);
  // A failed attempt leaves no handshake timer or statistics behind.
  const count = events.length;
  t.mock.timers.tick(20_000);
  assert.equal(events.length, count);

  // The retry connects normally and gets its own handshake deadline.
  await message(request({ canvas: { getContext: () => undefined } }));
  const socket = sockets[1];
  socket.readyState = 1;
  socket.onopen();
  assert.equal(socket.sent.length, 1);
  t.mock.timers.tick(15_000);
  assert.equal(closed().length, 4);
  assert.equal(closed()[3].code, 4002);
  assert.equal(socket.closed, true);
});

test("a close or a newer connect supersedes a connection still starting", async (t) => {
  const sockets = fakeSockets(t);
  t.mock.timers.enable({ apis: ["setTimeout", "setInterval"] });
  const events = [];
  const message = createSession((event) => events.push(event));
  t.after(() => message({ type: "close" }));
  const closed = () => events.filter((event) => event.type === "closed").map(({ code, reason }) => [code, reason]);

  // Setup waits for WebAssembly at least one turn; close during that wait.
  const first = message(request());
  await message({ type: "close" });
  await first;
  assert.equal(sockets.length, 0, "a closed request must not connect afterwards");
  assert.deepEqual(closed(), [[4000, "Disconnected."]]);

  const second = message(request());
  const third = message(request());
  await Promise.all([second, third]);
  assert.equal(sockets.length, 1, "only the newest request connects");
  assert.deepEqual(closed(), [[4000, "Disconnected."], [4000, "Replaced by a new connection."]]);
});
