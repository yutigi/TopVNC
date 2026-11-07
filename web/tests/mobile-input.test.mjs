import test from 'node:test';
import assert from 'node:assert/strict';
import { createMobileInput, keySym } from '../mobile-input.mjs';
import { createInput, MOTION_BACKLOG_BYTES } from '../session-input.mjs';
import { normalizeFpsKey, normalizeFpsSettings, defaultFpsSettings } from '../fps.mjs';

function fixture() {
  const sent = []; const socket = { bufferedAmount: 0 };
  const connection = {
    keySym: (sym, down) => sent.push(['key', sym, down]),
    typeText: text => sent.push(['text', text]),
    pointer: (mask, x, y) => sent.push(['pointer', mask, x, y]),
    pointerMotion: (mask, dx, dy) => sent.push(['motion', mask, dx, dy]),
    releaseAll: () => sent.push(['release']),
  };
  const session = createInput(connection, socket, { send() {}, fail: assert.fail });
  let mask = 0;
  const ui = createMobileInput({
    input: session.handle,
    button(bit, down) { mask = down ? mask | bit : mask & ~bit; session.handle({ kind: 'motion', buttons: mask, dx: 0, dy: 0, buttonsChanged: true }); },
    move(dx, dy) { session.handle({ kind: 'motion', buttons: mask, dx, dy }); },
  });
  return { sent, ui, session, socket };
}

test('one-shot chords cross the real input boundary in down/up order', () => {
  const { sent, ui } = fixture();
  ui.toggle('Ctrl'); ui.toggle('Alt'); ui.toggle('Shift');
  assert.deepEqual(sent, []);
  ui.tap('C');
  assert.deepEqual(sent, [
    ['key', 0xffe3, true], ['key', 0xffe9, true], ['key', 0xffe1, true], ['key', 99, true],
    ['key', 99, false], ['key', 0xffe1, false], ['key', 0xffe9, false], ['key', 0xffe3, false],
  ]);
  assert.equal(ui.modifiers.size, 0);
  ui.tap('Enter'); assert.deepEqual(sent.slice(-2), [['key', 0xff0d, true], ['key', 0xff0d, false]]);
});

test('independent holds, duplicate bindings and chord overlap retain ownership', () => {
  const { sent, ui } = fixture();
  ui.hold(1, 'W'); ui.hold(2, 'Mouse1'); ui.hold(3, 'w');
  ui.up(1); ui.tap('W');
  assert.deepEqual(sent, [['key', 119, true], ['motion', 1, 0, 0]]);
  ui.up(3); ui.up(2); ui.up(2);
  assert.deepEqual(sent.slice(-2), [['key', 119, false], ['motion', 0, 0, 0]]);
});

test('backpressure drops aim motion while preserving button and key releases', () => {
  const { sent, ui, socket } = fixture();
  socket.bufferedAmount = MOTION_BACKLOG_BYTES + 1;
  ui.hold(1, 'W'); ui.hold(2, 'Mouse1'); ui.move(10, 5); ui.release();
  assert.deepEqual(sent, [['key', 119, true], ['motion', 1, 0, 0], ['motion', 0, 0, 0], ['key', 119, false]]);
  ui.move(Infinity, NaN); assert.equal(sent.length, 4);
});

test('text clears latches, supports Unicode and enforces bounded insertion', () => {
  const { sent, ui } = fixture();
  ui.toggle('Ctrl'); ui.text('ไทย · 日本 · 中文 🎮');
  assert.deepEqual(sent, [['text', 'ไทย · 日本 · 中文 🎮']]);
  assert.equal(ui.modifiers.size, 0);
  assert.equal(ui.text('x'.repeat(2001)), false);
  assert.equal(ui.text(''), false); assert.equal(sent.length, 1);
});

test('long drafts are split at the Rust per-call limit without losing Unicode', () => {
  const { sent, ui } = fixture(); const value = 'x'.repeat(255) + '🎮' + 'ไทย'.repeat(200);
  ui.text(value);
  assert.equal(sent.map(message => message[1]).join(''), value);
  assert.ok(sent.every(message => [...message[1]].length <= 256));
});

test('key mapping rejects invalid bindings and bounds simultaneous owners', () => {
  assert.equal(keySym('F24'), 0xffd5); assert.equal(keySym('ไทย'), undefined);
  assert.equal(keySym('🎮'), 0x0101f3ae); assert.equal(keySym(null), undefined);
  const { sent, ui } = fixture();
  for (let i = 0; i < 33; i++) ui.hold(i, 'Space');
  ui.hold('bad', 'constructor'); ui.hold('bad', undefined);
  for (let i = 0; i < 32; i++) ui.up(i);
  assert.deepEqual(sent, [['key', 32, true], ['key', 32, false]]);
});

test('physical keys consume a latch and release its modifiers after their keyup', () => {
  const { sent, ui } = fixture();
  ui.toggle('Ctrl'); ui.toggle('Shift'); ui.beginPhysical('KeyC');
  assert.equal(ui.modifiers.size, 0);
  assert.deepEqual(sent, [['key', 0xffe3, true], ['key', 0xffe1, true]]);
  ui.endPhysical('KeyC');
  assert.deepEqual(sent.slice(-2), [['key', 0xffe1, false], ['key', 0xffe3, false]]);
});

test('FPS remaps validate keys, handedness and finite bounded sensitivity', () => {
  const result = normalizeFpsSettings({ handedness: 'left', sensitivity: 1.5, bindings: { forward: 'i', jump: 'space', fire: 'Mouse2', reload: 'constructor' } });
  assert.equal(result.bindings.forward, 'I'); assert.equal(result.bindings.jump, 'Space');
  assert.equal(result.bindings.fire, 'Mouse2'); assert.equal(result.bindings.reload, 'R');
  assert.equal(result.handedness, 'left'); assert.equal(result.sensitivity, 1.5);
  assert.deepEqual(normalizeFpsSettings({ handedness: 'invalid', sensitivity: Infinity }), defaultFpsSettings());
  assert.equal(normalizeFpsKey('Ctrl+R'), '');
});
