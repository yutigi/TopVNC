import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import { createCustomization } from '../dist/customize.mjs';
import { createLanguageSwitcher } from '../dist/language-switcher.mjs';

// Exercise the actual app handlers with a small DOM stand-in. This does not test rendering.
function app(saved = {}) {
  const nodes = [];
  const timers = new Map(); let nextTimer = 1; let pointed;
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
      contains(n) { return walk([this]).includes(n); },
      getBoundingClientRect() { return this.id === 'keyboard-languages' ? { left: 30, top: 620, bottom: 664, width: 44, height: 44 } : { left: 0, top: 0, width: 390, height: 740 }; },
      clientWidth: 378, clientHeight: 728, offsetWidth: 390, offsetHeight: 740, clientLeft: 6, clientTop: 6,
      setPointerCapture(id) { this.capture = id; }, hasPointerCapture(id) { return this.capture === id; }, releasePointerCapture() { this.capture = undefined; },
      scrollIntoView() {}, click() { emit(this, 'click'); },
    };
    return node;
  }
  function walk(list) { return list.flatMap(n => [n, ...walk(n.children)]); }
  function matches(node, selector) {
    if (selector.includes(',')) return selector.split(',').some(s => matches(node, s.trim()));
    if (selector.startsWith('#')) return node.id === selector.slice(1);
    if (selector.startsWith('.')) return node.classList.contains(selector.slice(1));
    const data = selector.match(/^\[data-([\w-]+)(?:="([^"]+)")?\]$/);
    if (data) { const key = data[1].replace(/-([a-z])/g, (_, c) => c.toUpperCase()); return node.dataset[key] !== undefined && (data[2] === undefined || node.dataset[key] === data[2]); }
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
    addEventListener(type, handler) { (this.listeners[type] ??= []).push(handler); }, elementFromPoint: () => pointed };
  const window = { listeners: {}, addEventListener(type, handler) { (this.listeners[type] ??= []).push(handler); }, setTimeout(fn) { const id = nextTimer++; timers.set(id, fn); return id; }, clearTimeout(id) { timers.delete(id); } };
  const store = new Map(Object.entries(saved));
  const context = vm.createContext({ document, window, localStorage: { getItem: k => store.get(k) ?? null, setItem: (k, v) => store.set(k, v) },
    navigator: { platform: 'test', userAgent: 'test' }, location: { search: '' }, innerWidth: 390, innerHeight: 740,
    URLSearchParams, structuredClone, setTimeout, clearTimeout, setInterval, clearInterval,
    createFpsController: () => ({ release() {}, apply() {} }),
    createCustomization: api => ({ apply() {}, usePreset() {}, isFps: () => false, build() {} }),
    createLanguageSwitcher: api => createLanguageSwitcher({ ...api, document, window }),
  });
  vm.runInContext(readFileSync(new URL('../dist/app.mjs', import.meta.url), 'utf8').replace(/^import .*\r?\n/gm, ''), context);
  function emit(target, type, event = {}) { for (const handler of target.listeners[type] ?? []) handler({ target, detail: 0, preventDefault() {}, stopPropagation() {}, ...event }); }
  function click(node) { emit(node, 'click'); emit(document, 'click', { target: node }); }
  function character(value) { const key = $$('[data-character]').find(n => n.dataset.character === value); assert.ok(key, `key ${value} exists`); click(key); }
  const modifier = name => $$('[data-mod]').find(n => n.dataset.mod === name);
  const evalApp = code => vm.runInContext(code, context);
  return { $, $$, document, window, emit, click, character, modifier, evalApp, element, store, point: node => { pointed = node; }, hold: () => { for (const [id, fn] of timers) { timers.delete(id); fn(); } }, timers };
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

function gesture(a, type, extra = {}) {
  a.emit(a.$('#keyboard-languages'), type, { button: 0, pointerId: 7, clientX: 40, clientY: 640, ...extra });
}

test('globe tap cycles once, preserves composition and ignores the compatibility click', () => {
  const a = app(); a.character('q');
  gesture(a, 'pointerdown'); gesture(a, 'pointerup');
  a.emit(a.$('#keyboard-languages'), 'click', { detail: 1 });
  assert.equal(a.$('#compose').lang, 'th');
  assert.equal(a.$('#compose').value, 'q');
  assert.equal(a.$('#language-popover').hidden, true);
  assert.equal(a.timers.size, 0);
  assert.equal(JSON.parse(a.store.get('topvnc-wireframe-v1')).selected, 'th');
});

test('holding, sliding and releasing chooses the highlighted language once', () => {
  const a = app(); a.character('q'); a.click(a.modifier('Ctrl'));
  gesture(a, 'pointerdown'); a.hold();
  assert.equal(a.modifier('Ctrl').attrs['aria-pressed'], 'false');
  assert.equal(a.$('#language-popover').hidden, false);
  const options = a.$$('[data-language-id]'); const japanese = options.find(n => n.dataset.languageId === 'ja');
  assert.equal(options[0].attrs['aria-checked'], 'true');
  a.point(japanese); gesture(a, 'pointermove', { clientY: 430 });
  assert.equal(japanese.classList.contains('language-hover'), true);
  assert.equal(japanese.attrs['aria-checked'], 'false');
  gesture(a, 'pointerup', { clientY: 430 });
  assert.equal(a.$('#compose').lang, 'ja'); assert.equal(a.$('#compose').value, 'q');
  assert.equal(a.$('#language-popover').hidden, true);
  a.emit(a.$('#keyboard-languages'), 'click', { detail: 1 });
  assert.equal(a.$('#compose').lang, 'ja');
});

test('lifting on globe keeps the list open; tapping a language or Manage works', () => {
  const a = app(); gesture(a, 'pointerdown'); a.hold(); a.point(a.$('#keyboard-languages')); gesture(a, 'pointerup');
  assert.equal(a.$('#language-popover').hidden, false);
  a.click(a.$$('[data-language-id]').find(n => n.dataset.languageId === 'zh'));
  assert.equal(a.$('#compose').lang, 'zh');
  a.click(a.$('#keyboard-languages')); a.click(a.$('[data-language-manage]'));
  assert.equal(a.$('#sheet').hidden, false); assert.equal(a.$('#sheet-title').textContent, 'Your languages');
  assert.equal(a.$('#language-popover').hidden, true);
});

test('sliding away early or releasing outside cancels without changing language', () => {
  const a = app(); gesture(a, 'pointerdown'); gesture(a, 'pointermove', { clientY: 600 }); a.hold(); gesture(a, 'pointerup');
  assert.equal(a.$('#compose').lang, 'en'); assert.equal(a.$('#language-popover').hidden, true);
  gesture(a, 'pointerdown'); a.hold(); a.point(a.$('#remote')); gesture(a, 'pointerup', { clientY: 20 });
  assert.equal(a.$('#compose').lang, 'en'); assert.equal(a.$('#language-popover').hidden, true);
});

test('keyboard navigation reaches languages and dismissal restores globe focus', () => {
  const a = app(); a.emit(a.$('#keyboard-languages'), 'keydown', { key: 'ArrowDown' });
  const options = a.$$('[data-language-id]'); assert.equal(a.document.activeElement, options[0]);
  a.emit(a.$('#language-popover'), 'keydown', { key: 'ArrowDown' }); assert.equal(a.document.activeElement, options[1]);
  a.emit(a.$('#language-popover'), 'keydown', { key: 'End' }); assert.equal(a.document.activeElement, a.$('[data-language-manage]'));
  a.emit(a.$('#language-popover'), 'keydown', { key: 'Escape' });
  assert.equal(a.document.activeElement, a.$('#keyboard-languages')); assert.equal(a.$('#language-popover').hidden, true);
});

test('mode, source, rotation, blur, pointer cancellation and outside press dismiss pending and open menus', () => {
  for (const cancel of [a => a.evalApp("setMode('keys')"), a => a.evalApp("setKeyboardSource('native')"), a => a.evalApp("setOrientation('landscape')"),
    a => a.emit(a.window, 'blur'), a => a.emit(a.window, 'pointercancel'), a => a.emit(a.document, 'pointerdown', { target: a.$('#remote') })]) {
    const a = app(); gesture(a, 'pointerdown'); a.hold(); cancel(a);
    assert.equal(a.$('#language-popover').hidden, true); assert.equal(a.$('#compose').lang, 'en');
    assert.equal(a.$('#keyboard-languages').capture, undefined);
  }
  const a = app(); gesture(a, 'pointerdown'); a.emit(a.window, 'blur'); a.hold();
  assert.equal(a.$('#language-popover').hidden, true);
});

test('scrolling a long language menu remains possible while deck scrolling dismisses it', () => {
  const a = app(); a.click(a.$('#keyboard-languages'));
  a.emit(a.$('#device'), 'scroll', { target: a.$('.language-popover-list') });
  assert.equal(a.$('#language-popover').hidden, false);
  a.emit(a.$('#device'), 'scroll', { target: a.$('.control-deck') });
  assert.equal(a.$('#language-popover').hidden, true);
});

test('IME composition blocks language gestures without losing composed text', () => {
  const a = app(); a.evalApp("setKeyboardSource('native')"); a.$('#compose').value = '日本';
  a.emit(a.$('#compose'), 'compositionstart'); gesture(a, 'pointerdown'); a.hold(); a.click(a.$('#keyboard-languages'));
  assert.equal(a.$('#language-popover').hidden, true); assert.equal(a.$('#compose').value, '日本'); assert.equal(a.$('#compose').lang, 'en');
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
  assert.equal(a.$('#language-popover').hidden, false);
  assert.equal(a.$('#keyboard-languages').attrs['aria-expanded'], 'true');
  assert.match(a.$('#keyboard-languages').attrs['aria-label'], /My layout selected/);
  a.evalApp('release()');
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
