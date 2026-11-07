import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { createCustomization } from '../dist/customize.mjs';

// Exercise the actual app handlers with a small DOM stand-in. This does not test rendering.
function app(saved = {}) {
  const nodes = [];
  function element(tag = 'div') {
    const attrs = {}; const classes = new Set(); const listeners = {};
    const node = {
      tag, attrs, listeners, dataset: {}, style: {}, children: [], value: '', hidden: false, isConnected: true,
      classList: { add: c => classes.add(c), remove: c => classes.delete(c), toggle(c, on = !classes.has(c)) { on ? classes.add(c) : classes.delete(c); }, contains: c => classes.has(c) },
      set className(value) { classes.clear(); value.split(/\s+/).forEach(c => classes.add(c)); },
      setAttribute(key, value) { attrs[key] = value; }, removeAttribute(key) { delete attrs[key]; },
      addEventListener(type, handler) { (listeners[type] ??= []).push(handler); },
      append(...items) { this.children.push(...items); }, replaceChildren(...items) { this.children = items; },
      focus() { document.activeElement = this; }, blur() { document.activeElement = document.body; },
      closest(selector) { return matches(this, selector) ? this : null; },
      querySelector(selector) { return walk(this.children).find(n => matches(n, selector)); },
      querySelectorAll(selector) { return walk(this.children).filter(n => matches(n, selector)); },
    };
    return node;
  }
  function walk(list) { return list.flatMap(n => [n, ...walk(n.children)]); }
  function matches(node, selector) {
    if (selector.startsWith('#')) return node.id === selector.slice(1);
    if (selector.startsWith('.')) return node.classList.contains(selector.slice(1));
    const data = selector.match(/^\[data-([\w-]+)(?:="([^"]+)")?\]$/);
    if (data) return node.dataset[data[1]] !== undefined && (data[2] === undefined || node.dataset[data[1]] === data[2]);
    return node.tag === selector;
  }
  const html = readFileSync(new URL('../dist/index.html', import.meta.url), 'utf8');
  for (const match of html.matchAll(/<([\w-]+)\b([^>]*)>/g)) {
    const node = element(match[1]);
    for (const [, key, value = ''] of match[2].matchAll(/([\w-]+)(?:="([^"]*)")?/g)) {
      if (key === 'id') node.id = value;
      else if (key === 'class') node.className = value;
      else if (key.startsWith('data-')) node.dataset[key.slice(5)] = value;
      else if (key === 'hidden') node.hidden = true;
      else node.setAttribute(key, value);
    }
    nodes.push(node);
  }
  const all = () => walk(nodes);
  const $ = selector => all().find(n => matches(n, selector));
  const $$ = selector => all().filter(n => matches(n, selector));
  const document = { body: $('body'), activeElement: $('body'), listeners: {}, createElement: element, querySelector: $, querySelectorAll: $$,
    addEventListener(type, handler) { (this.listeners[type] ??= []).push(handler); } };
  const window = { listeners: {}, addEventListener(type, handler) { (this.listeners[type] ??= []).push(handler); } };
  const store = new Map(Object.entries(saved));
  const context = vm.createContext({ document, window, localStorage: { getItem: k => store.get(k) ?? null, setItem: (k, v) => store.set(k, v) },
    navigator: { platform: 'test', userAgent: 'test' }, location: { search: '' }, innerWidth: 390, innerHeight: 740,
    URLSearchParams, structuredClone, setTimeout, clearTimeout, setInterval, clearInterval,
    createFpsController: () => ({ release() {}, apply() {} }),
    createCustomization: api => ({ apply() {}, usePreset() {}, isFps: () => false, build() {} }),
  });
  vm.runInContext(readFileSync(new URL('../dist/app.mjs', import.meta.url), 'utf8').replace(/^import .*\r?\n/gm, ''), context);
  function emit(target, type, event = {}) { for (const handler of target.listeners[type] ?? []) handler({ target, detail: 0, preventDefault() {}, ...event }); }
  function click(node) { emit(node, 'click'); emit(document, 'click', { target: node }); }
  function character(value) { const key = $$('[data-character]').find(n => n.dataset.character === value); assert.ok(key, `key ${value} exists`); click(key); }
  const modifier = name => $$('[data-mod]').find(n => n.dataset.mod === name);
  const evalApp = code => vm.runInContext(code, context);
  return { $, $$, document, window, emit, click, character, modifier, evalApp, element, store };
}

test('Shift controls share state, update English/Thai labels and clear after text', () => {
  const a = app();
  a.click(a.modifier('Shift'));
  assert.ok(a.$$('[data-mod]').filter(n => n.dataset.mod === 'Shift').every(n => n.attrs['aria-pressed'] === 'true'));
  a.character('Q');
  assert.equal(a.$('#compose').value, 'Q');
  assert.equal(a.modifier('Shift').attrs['aria-pressed'], 'false');
  a.character('q');
  assert.equal(a.$('#compose').value, 'Qq');
  a.evalApp("selectLanguage('th')");
  a.click(a.modifier('Shift')); a.character('๐');
  assert.equal(a.$('#compose').value, 'Qq๐');
  const inlineShift = a.$('.keyboard-shift'); a.click(inlineShift);
  assert.equal(a.modifier('Shift').attrs['aria-pressed'], 'true');
});

test('Ctrl/Alt chords, Space, Backspace and Return leave composed text intact', () => {
  const a = app(); a.character('c');
  a.click(a.modifier('Ctrl')); a.click(a.modifier('Alt')); a.click(a.modifier('Shift')); a.character('C');
  assert.equal(a.$('#compose').value, 'c');
  assert.equal(a.$('#key-feedback').textContent, 'Ctrl + Alt + Shift + C · demo press');
  assert.equal(a.modifier('Ctrl').attrs['aria-pressed'], 'false');
  for (const [target, expected] of [['#keyboard-space', 'Space'], ['#keyboard-backspace', 'Backspace'], ['#keyboard-return', 'Enter']]) {
    a.click(a.modifier('Ctrl')); a.click(a.$(target));
    assert.equal(a.$('#compose').value, 'c');
    assert.equal(a.$('#key-feedback').textContent, `Ctrl + ${expected} · demo press`);
  }
  a.click(a.$('#keyboard-return'));
  assert.equal(a.$('#remote-output').textContent, 'c');
  assert.equal(a.$('#compose').value, '');
});

test('mode, orientation, sheets, cancellation and focus loss release every modifier', () => {
  const a = app();
  for (const action of [() => a.evalApp("setMode('keys')"), () => a.evalApp("setOrientation('landscape')"), () => a.click(a.$('#keyboard-languages')),
    ...['blur', 'pointercancel', 'pagehide'].map(type => () => a.emit(a.window, type)), () => a.emit(a.document, 'visibilitychange')]) {
    a.click(a.modifier('Ctrl')); a.click(a.modifier('Shift')); action();
    assert.ok(a.$$('[data-mod]').every(n => n.attrs['aria-pressed'] === 'false'));
  }
});

test('native composition blocks shortcuts, switching profiles and inserting text', () => {
  const a = app(); a.evalApp("setKeyboardSource('native')");
  a.$('#compose').value = '日本';
  a.emit(a.$('#compose'), 'compositionstart');
  a.evalApp("pressKey('Ctrl+C'); selectLanguage('th'); insertText()");
  assert.equal(a.$('#compose').lang, 'en');
  assert.equal(a.$('#remote-output').textContent, undefined);
  assert.equal(a.$('#compose').value, '日本');
  a.emit(a.$('#compose'), 'compositionend'); a.click(a.$('#insert-text'));
  assert.equal(a.$('#remote-output').textContent, '日本');
});

test('keyboard icon opens the picker and preserves saved profiles and editable rows', () => {
  const profiles = [{ id: 'mine', name: 'My layout', label: 'Mine', tag: 'und', pinned: true, rows: [['x', 'y', 'z'], ['a', 'b', 'c'], ['d', 'e', 'f']] }];
  const a = app({ 'topvnc-wireframe-v1': JSON.stringify({ profiles, selected: 'mine' }) });
  a.click(a.$('#keyboard-languages'));
  assert.equal(a.$('#sheet').hidden, false);
  assert.equal(a.$('#sheet-title').textContent, 'Your languages');
  assert.match(a.$('#keyboard-languages').attrs['aria-label'], /My layout selected/);
  a.evalApp('closeSheet()');
  a.character('x'); assert.equal(a.$('#compose').value, 'x');
  assert.equal(a.$('#language-pins'), undefined);
});

test('touch character chords use the same path and do not start Backspace repeat', () => {
  const a = app(); a.character('c'); a.click(a.modifier('Ctrl'));
  a.emit(a.document, 'pointerdown', { target: a.$$('[data-character]').find(n => n.dataset.character === 'c') });
  assert.equal(a.$('#key-feedback').textContent, 'Ctrl + C · demo press');
  assert.equal(a.$('#compose').value, 'c');
  a.click(a.modifier('Ctrl'));
  const key = a.$('#keyboard-backspace');
  a.emit(key, 'pointerdown', { currentTarget: { setPointerCapture() {} }, pointerId: 1 });
  assert.equal(a.$('#key-feedback').textContent, 'Ctrl + Backspace · demo press');
  assert.equal(a.evalApp('eraseDelay'), undefined);
});

test('actual layout application keeps migrated modifiers Type-only even with old hide preference', () => {
  const a = app({ 'topvnc-wireframe-customize-v1': JSON.stringify({ layouts: { portrait: { showLanguages: false } } }) });
  let mode = 'type';
  const dock = a.$('.thumb-dock');
  for (const button of [...a.$$('[data-mode]'), a.$('.dock-enter')]) { button.append(a.element('small'), a.element('span')); }
  const priorStorage = globalThis.localStorage;
  globalThis.localStorage = { getItem: k => a.store.get(k) ?? null };
  try {
    const customization = createCustomization({ $: a.$, el: (tag, text) => Object.assign(a.element(tag), { textContent: text }), orientation: () => 'portrait', mode: () => mode, fps() {} });
    customization.apply(); assert.equal(a.$('.type-modifiers').hidden, false);
    for (const next of ['keys', 'pointer']) { mode = next; customization.apply(); assert.equal(a.$('.type-modifiers').hidden, true); }
    mode = 'type'; customization.apply(); assert.equal(a.$('.type-modifiers').hidden, false);
    assert.ok(dock.children.length > 0);
  } finally { globalThis.localStorage = priorStorage; }
});
