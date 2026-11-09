import test from 'node:test';
import assert from 'node:assert/strict';
import { createHeldInputs, defaultFpsSettings, normalizeFpsSettings, normalizeFpsKey } from '../dist/fps.mjs';
import { defaultCustomization, normalizeCustomization } from '../dist/customize.mjs';

test('movement and firing holds release independently', () => {
  const held = createHeldInputs();
  held.down(10, 'W'); held.down(11, 'Mouse1'); held.down(12, 'Shift');
  assert.deepEqual(held.keys(), ['W', 'Mouse1', 'Shift']);
  held.up(11);
  assert.deepEqual(held.keys(), ['W', 'Shift']);
  held.clear();
  assert.deepEqual(held.keys(), []);
});

test('two fingers bound to the same key keep it held until both release', () => {
  const held = createHeldInputs();
  held.down(1, 'Space'); held.down(2, 'Space'); held.up(1);
  assert.deepEqual(held.keys(), ['Space']);
  held.up(2);
  assert.deepEqual(held.keys(), []);
});

test('held input is bounded and rejects invalid bindings', () => {
  const held = createHeldInputs();
  for (let i = 0; i < 16; i++) held.down(i, 'W');
  held.down(17, 'Mouse1'); held.down(0, 'constructor');
  assert.deepEqual(held.keys(), ['W']);
  held.up(0); held.down(17, 'Mouse1');
  assert.deepEqual(held.keys(), ['W', 'Mouse1']);
});

test('FPS settings validate persisted remaps and sensitivity', () => {
  const settings = normalizeFpsSettings({ handedness: 'left', sensitivity: 1.5, bindings: { forward: 'i', jump: 'space', fire: 'Mouse2', sprint: 'shift', reload: 'constructor' } });
  assert.equal(settings.bindings.forward, 'I');
  assert.equal(settings.bindings.jump, 'Space');
  assert.equal(settings.bindings.sprint, 'Shift');
  assert.equal(settings.bindings.reload, 'R');
  assert.equal(settings.handedness, 'left');
  assert.equal(settings.sensitivity, 1.5);
  assert.deepEqual(normalizeFpsSettings({ handedness: 'invalid', sensitivity: Infinity }), defaultFpsSettings());
  assert.equal(normalizeFpsKey('Ctrl+R'), '');
});

test('FPS selection round-trips without replacing custom keys or standard layouts', () => {
  const saved = defaultCustomization();
  saved.preset = 'fps';
  saved.layouts.landscape.position = 'left';
  saved.actions = [{ id: 'use', label: 'Use', type: 'key', value: 'E', wide: false }];
  saved.fps.bindings.interact = 'F';
  const result = normalizeCustomization(JSON.parse(JSON.stringify(saved)));
  assert.equal(result.preset, 'fps');
  assert.equal(result.layouts.landscape.position, 'left');
  assert.deepEqual(result.actions, saved.actions);
  assert.equal(result.fps.bindings.interact, 'F');
  assert.equal(normalizeCustomization({ preset: 'unknown' }).preset, 'standard');
});
