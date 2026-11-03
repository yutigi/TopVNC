import test, { mock } from "node:test";
import assert from "node:assert/strict";
import { MOTION_BACKLOG_BYTES, MOTION_RETRY_MS, createInput } from "../session-input.mjs";

const CONGESTED = MOTION_BACKLOG_BYTES + 1;

// A Connection that records the input it is given, and a socket whose
// backlog the test sets.
function fixture() {
  const calls = [];
  const record = (name) => (...args) => calls.push([name, ...args]);
  const connection = {
    pointer: record("pointer"),
    pointerMotion: record("pointerMotion"),
    keyCode: record("keyCode"),
    keySym: record("keySym"),
    typeText: record("typeText"),
    releaseAll: record("releaseAll"),
    setQuality: record("setQuality"),
    requestServeScale: record("requestServeScale"),
    requestServeDisplay: record("requestServeDisplay"),
  };
  const socket = { bufferedAmount: 0 };
  const failures = [];
  const sends = { count: 0 };
  const input = createInput(connection, socket, {
    send: () => sends.count++,
    fail: (error) => failures.push(error),
  });
  return { calls, connection, socket, input, failures, sends };
}

const drag = (x, y) => ({ kind: "pointer", buttons: 1, x, y, motion: true });
const pointers = (calls) => calls.filter(([name]) => name === "pointer");

function withTimers(body) {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    body();
  } finally {
    mock.timers.reset();
  }
}

test("a release after the backlog drains does not replay held motion with its buttons", () => {
  withTimers(() => {
    const { calls, socket, input } = fixture();
    socket.bufferedAmount = 70_000;
    input.handle(drag(100, 120));
    assert.deepEqual(calls, []);
    // The socket drains, then the page loses focus before the retry runs.
    socket.bufferedAmount = 0;
    input.handle({ kind: "release" });
    mock.timers.tick(MOTION_RETRY_MS * 4);
    input.handle({ kind: "code", code: "KeyA", down: true });
    assert.deepEqual(calls, [["releaseAll"], ["keyCode", "KeyA", true]]);
  });
});

test("a release while still congested discards held motion for good", () => {
  withTimers(() => {
    const { calls, socket, input } = fixture();
    socket.bufferedAmount = CONGESTED;
    input.handle(drag(100, 120));
    input.handle({ kind: "release" });
    socket.bufferedAmount = 0;
    mock.timers.tick(MOTION_RETRY_MS * 4);
    assert.deepEqual(calls, [["releaseAll"]]);
  });
});

test("held motion is sent once the socket drains, without further input", () => {
  withTimers(() => {
    const { calls, socket, input, sends } = fixture();
    socket.bufferedAmount = CONGESTED;
    input.handle(drag(10, 10));
    input.handle(drag(20, 20));
    input.handle(drag(30, 30));
    mock.timers.tick(MOTION_RETRY_MS);
    assert.deepEqual(calls, [], "still congested");
    socket.bufferedAmount = 0;
    mock.timers.tick(MOTION_RETRY_MS);
    // Only the latest position, once.
    assert.deepEqual(calls, [["pointer", 1, 30, 30]]);
    assert.equal(sends.count, 1);
    mock.timers.tick(MOTION_RETRY_MS * 4);
    assert.deepEqual(calls, [["pointer", 1, 30, 30]]);
    // A release after the motion was sent still releases.
    input.handle({ kind: "release" });
    assert.deepEqual(calls.at(-1), ["releaseAll"]);
  });
});

test("button changes are never held and replace stale held motion", () => {
  withTimers(() => {
    const { calls, socket, input } = fixture();
    socket.bufferedAmount = CONGESTED;
    input.handle(drag(10, 10));
    input.handle({ kind: "pointer", buttons: 0, x: 12, y: 12 });
    socket.bufferedAmount = 0;
    mock.timers.tick(MOTION_RETRY_MS * 4);
    assert.deepEqual(pointers(calls), [["pointer", 0, 12, 12]]);
  });
});

test("relative button changes are sent under backpressure and drop held absolute motion", () => {
  withTimers(() => {
    const { calls, socket, input } = fixture();
    socket.bufferedAmount = CONGESTED;
    input.handle(drag(10, 10));
    input.handle({ kind: "motion", buttons: 0, dx: 1, dy: 0 });
    input.handle({ kind: "motion", buttons: 0, dx: 1, dy: 0, buttonsChanged: true });
    socket.bufferedAmount = 0;
    mock.timers.tick(MOTION_RETRY_MS * 4);
    assert.deepEqual(calls, [["pointerMotion", 0, 1, 0]]);
  });
});

test("other input flushes held motion as soon as the socket has drained", () => {
  withTimers(() => {
    const { calls, socket, input } = fixture();
    socket.bufferedAmount = CONGESTED;
    input.handle({ ...drag(40, 50), buttons: 0 });
    socket.bufferedAmount = 0;
    input.handle({ kind: "keysym", keysym: 0xff0d, down: true });
    assert.deepEqual(calls, [["keySym", 0xff0d, true], ["pointer", 0, 40, 50]]);
    mock.timers.tick(MOTION_RETRY_MS * 4);
    assert.equal(pointers(calls).length, 1);
  });
});

test("discarding on close cancels the retry", () => {
  withTimers(() => {
    const { calls, socket, input } = fixture();
    socket.bufferedAmount = CONGESTED;
    input.handle(drag(10, 10));
    input.discard();
    socket.bufferedAmount = 0;
    mock.timers.tick(MOTION_RETRY_MS * 4);
    assert.deepEqual(calls, []);
  });
});

test("a failure while retrying is reported, not thrown from the timer", () => {
  withTimers(() => {
    const { connection, socket, input, failures } = fixture();
    connection.pointer = () => {
      throw new Error("closed");
    };
    socket.bufferedAmount = CONGESTED;
    input.handle(drag(10, 10));
    socket.bufferedAmount = 0;
    mock.timers.tick(MOTION_RETRY_MS);
    assert.equal(failures.length, 1);
    assert.equal(failures[0].message, "closed");
  });
});
