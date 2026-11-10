import test from 'node:test';
import assert from 'node:assert/strict';
import { defaultCustomization, normalizeChord, normalizeCustomization, moveItem } from '../dist/customize.mjs';

test('shortcut aliases normalize while invalid chords are rejected', () => {
  assert.equal(normalizeChord(' control + shift + s '), 'Ctrl+Shift+S');
  assert.equal(normalizeChord('Ctrl+Alt+Del'), 'Ctrl+Alt+Delete');
  assert.equal(normalizeChord('F24'), 'F24');
  for (const chord of ['Ctrl++S', 'Ctrl+Ctrl+C', 'C+Ctrl', 'Ctrl', 'F25', 'constructor', '__proto__', 'eval(alert(1))']) assert.equal(normalizeChord(chord), '');
});

test('saved custom keys and independent layouts round-trip without sharing references', () => {
  const input = defaultCustomization();
  input.actions = [{ id: 'save', label: 'Save as', type: 'key', value: 'ctrl+shift+s', wide: true }, { id: 'greeting', label: 'Hello', type: 'text', value: '你好 · こんにちは · สวัสดี', wide: false }];
  input.layouts.landscape.position = 'left';
  input.layouts.landscape.dock = ['enter', 'pointer', 'type', 'keys'];
  input.layouts.portrait.showLanguages = false;
  const result = normalizeCustomization(JSON.parse(JSON.stringify(input)));
  assert.equal(result.actions[0].value, 'Ctrl+Shift+S');
  assert.equal(result.actions[1].value, input.actions[1].value);
  assert.equal(result.layouts.landscape.position, 'left');
  assert.equal(result.layouts.portrait.position, 'auto');
  assert.equal(Object.hasOwn(result.layouts.portrait, 'showLanguages'), false);
  result.layouts.landscape.dock.reverse();
  assert.deepEqual(input.layouts.landscape.dock, ['enter', 'pointer', 'type', 'keys']);
});

test('malformed stored ordering cannot remove controls or inject placement styles', () => {
  const result = normalizeCustomization({ layouts: { portrait: { dock: ['keys', 'keys', 'keys', 'keys'], groups: ['input'], position: 'left;display:none', size: '0px' } } });
  assert.deepEqual(result.layouts.portrait.dock, ['keys', 'type', 'pointer', 'enter']);
  assert.deepEqual(result.layouts.portrait.groups, ['modifiers', 'actions', 'input']);
  assert.equal(result.layouts.portrait.position, 'auto');
  assert.equal(result.layouts.portrait.size, 'standard');
});

test('stored actions reject duplicates, oversized text, and invalid action types', () => {
  const action = { id: 'a', label: 'A', type: 'text', value: 'hello', wide: false };
  for (const actions of [[action, action], [{ ...action, value: 'x'.repeat(201) }], [{ ...action, type: 'script' }], Array.from({ length: 9 }, (_, i) => ({ ...action, id: String(i) }))]) {
    assert.deepEqual(normalizeCustomization({ actions }).actions, []);
  }
});

test('reordering preserves every control and never mutates the original list', () => {
  const dock = ['keys', 'type', 'pointer', 'enter'];
  assert.deepEqual(moveItem(dock, 1, -1), ['type', 'keys', 'pointer', 'enter']);
  assert.deepEqual(moveItem(dock, 0, -1), dock);
  assert.deepEqual(moveItem(dock, 3, 1), dock);
  assert.deepEqual(moveItem(dock, 0, 100), dock);
  assert.deepEqual(dock, ['keys', 'type', 'pointer', 'enter']);
});

test('default reset objects do not change the other orientation', () => {
  const state = defaultCustomization();
  state.layouts.portrait.groups.reverse();
  assert.deepEqual(state.layouts.landscape.groups, ['modifiers', 'actions', 'input']);
  assert.deepEqual(normalizeCustomization(null), defaultCustomization());
});

test('legacy language row placement migrates without hiding Type modifiers', () => {
  const result = normalizeCustomization({ layouts: {
    portrait: { groups: ['actions', 'input', 'languages'], showLanguages: false },
    landscape: { groups: ['languages', 'input', 'actions'], showLanguages: true },
  } });
  assert.deepEqual(result.layouts.portrait.groups, ['actions', 'input', 'modifiers']);
  assert.deepEqual(result.layouts.landscape.groups, ['modifiers', 'input', 'actions']);
  assert.equal(Object.hasOwn(result.layouts.portrait, 'showLanguages'), false);
  assert.deepEqual(normalizeCustomization({ layouts: { portrait: { groups: { map: 'bad' } } } }).layouts.portrait.groups, ['modifiers', 'actions', 'input']);
});
