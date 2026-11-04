import test, { mock } from "node:test";
import assert from "node:assert/strict";
import { LONG_PRESS_MS, SCROLL_STEP_PX, TouchGestures } from "../gestures.mjs";

function recorder() {
  const log = [];
  const actions = {
    move: (to) => log.push(["move", to]),
    button: (name, down) => log.push([down ? "down" : "up", name]),
    scroll: (dx, dy) => log.push(["scroll", dx, dy]),
    zoom: (factor) => log.push(["zoom", Number(factor.toFixed(2))]),
    pan: (dx, dy) => log.push(["pan", Math.round(dx), Math.round(dy)]),
    released: () => log.push(["released"]),
  };
  return { log, actions };
}

const clicks = (log) => log.filter(([kind]) => kind === "down" || kind === "up");

test("trackpad: a tap clicks, a drag moves relatively, tap-then-drag holds the button", () => {
  const { log, actions } = recorder();
  const touch = new TouchGestures(actions);
  touch.down(1, 10, 10, 0);
  touch.up(1, 11, 10, 100);
  assert.deepEqual(clicks(log), [["down", "left"], ["up", "left"]]);
  log.length = 0;
  // Long after the tap: moving only moves.
  touch.down(1, 10, 10, 1000);
  touch.move(1, 30, 15, 1050);
  touch.move(1, 35, 15, 1060);
  touch.up(1, 35, 15, 1100);
  assert.deepEqual(log.filter(([kind]) => kind === "move").map(([, to]) => to), [{ dx: 20, dy: 5 }, { dx: 5, dy: 0 }]);
  assert.deepEqual(clicks(log), []);
  log.length = 0;
  // Tap, then touch again and drag: the left button is held for the drag.
  touch.down(1, 10, 10, 2000);
  touch.up(1, 10, 10, 2080);
  touch.down(1, 10, 10, 2200);
  touch.move(1, 40, 10, 2250);
  touch.up(1, 40, 10, 2300);
  assert.deepEqual(clicks(log), [["down", "left"], ["up", "left"], ["down", "left"], ["up", "left"]]);
});

test("two fingers: a tap right-clicks, a drag scrolls, a pinch zooms and pans", () => {
  const { log, actions } = recorder();
  const touch = new TouchGestures(actions);
  touch.down(1, 100, 100, 0);
  touch.down(2, 140, 100, 10);
  touch.up(1, 100, 100, 120);
  touch.up(2, 140, 100, 130);
  assert.deepEqual(clicks(log), [["down", "right"], ["up", "right"]]);
  log.length = 0;
  // Fingers move up together by three notches, reporting one at a time in
  // small steps as touch screens do: scroll down three.
  touch.down(1, 100, 300, 1000);
  touch.down(2, 140, 300, 1005);
  for (let moved = 2; moved <= 3 * SCROLL_STEP_PX; moved += 2) {
    touch.move(1, 100, 300 - moved, 1000 + moved);
    touch.move(2, 140, 300 - moved, 1001 + moved);
  }
  touch.up(1, 100, 216, 1100);
  // The remaining finger alone does nothing, then lifts without a click.
  touch.move(2, 200, 100, 1110);
  touch.up(2, 200, 100, 1120);
  const scrolled = log.filter(([kind]) => kind === "scroll").reduce((sum, [, , dy]) => sum + dy, 0);
  assert.equal(scrolled, 3);
  assert.deepEqual(clicks(log), []);
  log.length = 0;
  // Spreading the fingers zooms the local view instead of scrolling.
  touch.down(1, 100, 100, 2000);
  touch.down(2, 140, 100, 2005);
  for (let spread = 10; spread <= 40; spread += 10) {
    touch.move(1, 100 - spread / 2, 100, 2000 + spread);
    touch.move(2, 140 + spread / 2, 100, 2001 + spread);
  }
  touch.up(1, 80, 100, 2100);
  touch.up(2, 160, 100, 2110);
  const zoomed = log.filter(([kind]) => kind === "zoom").reduce((product, [, factor]) => product * factor, 1);
  assert.ok(zoomed > 1.8, `zoomed by ${zoomed}`);
  assert.ok(!log.some(([kind]) => kind === "scroll"));
  assert.deepEqual(clicks(log), []);
});

test("two fingers moving together at a realistic spacing scroll, never zoom", () => {
  const { log, actions } = recorder();
  const touch = new TouchGestures(actions);
  // Diagonal spacing and 10 px per report: each half-updated pair differs
  // from the starting span by more than the pinch threshold.
  touch.down(1, 100, 300, 0);
  touch.down(2, 140, 260, 1);
  for (let moved = 10; moved <= 90; moved += 10) {
    touch.move(1, 100, 300 - moved, moved);
    touch.move(2, 140, 260 - moved, moved + 1);
  }
  touch.up(1, 100, 210, 100);
  touch.up(2, 140, 170, 101);
  assert.ok(!log.some(([kind]) => kind === "zoom" || kind === "pan"), JSON.stringify(log));
  const scrolled = log.filter(([kind]) => kind === "scroll").reduce((sum, [, , dy]) => sum + dy, 0);
  assert.equal(scrolled, Math.trunc(90 / SCROLL_STEP_PX));
  assert.deepEqual(clicks(log), []);
});

test("a pinch with one finger held still zooms", () => {
  const { log, actions } = recorder();
  const touch = new TouchGestures(actions);
  touch.down(1, 100, 100, 0);
  touch.down(2, 140, 100, 1);
  // Only the moving finger reports.
  for (let x = 150; x <= 200; x += 10) touch.move(2, x, 100, x);
  touch.up(2, 200, 100, 300);
  touch.up(1, 100, 100, 301);
  const zoomed = log.filter(([kind]) => kind === "zoom").reduce((product, [, factor]) => product * factor, 1);
  assert.ok(zoomed > 2, `zoomed by ${zoomed}`);
  assert.ok(!log.some(([kind]) => kind === "scroll"));
});

test("direct: taps click where touched, drags hold the button, long press right-clicks", () => {
  mock.timers.enable({ apis: ["setTimeout"] });
  try {
    const { log, actions } = recorder();
    const touch = new TouchGestures(actions, "direct");
    touch.down(1, 50, 60, 0);
    touch.up(1, 50, 60, 100);
    assert.deepEqual(log.slice(0, 4), [["move", { x: 50, y: 60 }], ["move", { x: 50, y: 60 }], ["down", "left"], ["up", "left"]]);
    log.length = 0;
    touch.down(1, 10, 10, 1000);
    touch.move(1, 50, 10, 1100);
    touch.up(1, 50, 10, 1200);
    assert.deepEqual(clicks(log), [["down", "left"], ["up", "left"]]);
    assert.deepEqual(log.at(-3), ["move", { x: 50, y: 10 }]);
    log.length = 0;
    touch.down(1, 70, 80, 3000);
    mock.timers.tick(LONG_PRESS_MS + 1);
    touch.up(1, 70, 80, 3000 + LONG_PRESS_MS + 50);
    assert.deepEqual(clicks(log), [["down", "right"], ["up", "right"]]);
  } finally {
    mock.timers.reset();
  }
});

test("cancelling releases a held button", () => {
  const { log, actions } = recorder();
  const touch = new TouchGestures(actions, "direct");
  touch.down(1, 10, 10, 0);
  touch.move(1, 60, 10, 50);
  touch.cancel();
  assert.deepEqual(clicks(log), [["down", "left"], ["up", "left"]]);
  assert.equal(log.at(-1)[0], "released");
  // A later finger lift does nothing.
  log.length = 0;
  touch.up(1, 60, 10, 80);
  assert.deepEqual(log, []);
});
