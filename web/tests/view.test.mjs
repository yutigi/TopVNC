import test from "node:test";
import assert from "node:assert/strict";
import { MAX_ZOOM, View, fit } from "../view.mjs";

test("fits the framebuffer with its aspect ratio and maps points both ways", () => {
  assert.deepEqual(fit(400, 400, 1920, 1080), { width: 400, height: 225, left: 0, top: 87.5 });
  const view = new View();
  view.layout(400, 400, 1920, 1080);
  assert.deepEqual(view.toFrame(0, 87.5, 1920, 1080), { x: 0, y: 0 });
  assert.deepEqual(view.toFrame(200, 200, 1920, 1080), { x: 960, y: 540 });
  // Points outside the image clamp to its edge.
  assert.deepEqual(view.toFrame(-50, 10, 1920, 1080), { x: 0, y: 0 });
  assert.deepEqual(view.toFrame(999, 999, 1920, 1080), { x: 1919, y: 1079 });
  const back = view.fromFrame(960, 540, 1920, 1080);
  assert.ok(Math.abs(back.x - 200) < 1 && Math.abs(back.y - 200) < 1);
});

test("zooms around the pinch point and keeps the image covering the viewer", () => {
  const view = new View();
  view.layout(400, 225, 1920, 1080);
  const before = view.toFrame(300, 100, 1920, 1080);
  view.zoomAt(2, 300, 100);
  assert.equal(view.zoom, 2);
  const after = view.toFrame(300, 100, 1920, 1080);
  assert.ok(Math.abs(after.x - before.x) <= 2 && Math.abs(after.y - before.y) <= 2);
  view.pan(10_000, 10_000);
  assert.deepEqual([view.x, view.y], [0, 0]);
  view.pan(-10_000, -10_000);
  assert.deepEqual([view.x, view.y], [-400, -225]);
  view.zoomAt(100, 0, 0);
  assert.equal(view.zoom, MAX_ZOOM);
  view.zoomAt(0.001, 0, 0);
  assert.deepEqual([view.zoom, view.x, view.y], [1, 0, 0]);
  assert.equal(view.framePerViewPixel(1920), 1920 / 400);
});
