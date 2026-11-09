import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { buildSync } from 'esbuild';
import vm from 'node:vm';

// Run the real UI modules and markup with a DOM stand-in. This verifies event
// routing and state, not rendering, browser pointer capture or device IMEs.
const bundle = buildSync({ entryPoints: [fileURLToPath(new URL('../mobile-controls.mjs', import.meta.url))], bundle: true, write: false, format: 'iife', globalName: 'mobileUI' }).outputFiles[0].text;
function app(saved = {}) {
  let pointed; let nextTimer = 1; let connected = true;
  const timers = new Map(); const store = new Map(Object.entries(saved)); const sent = [];
  const document = { activeElement: undefined, listeners: {}, createElement: element, elementFromPoint: () => pointed, addEventListener: listen };
  const window = { listeners: {}, addEventListener: listen, setTimeout(fn) { const id = nextTimer++; timers.set(id, fn); return id; }, clearTimeout(id) { timers.delete(id); } };
  const landscape = { matches: false, listeners: {}, addEventListener: listen };
  function listen(name, handler, capture = false) { (this.listeners[name] ??= []).push({ handler, capture }); }
  function walk(nodes) { return nodes.flatMap(node => [node, ...walk(node.children)]); }
  function matches(node, selector) {
    if (selector.includes(',')) return selector.split(',').some(value => matches(node, value.trim()));
    if (selector.startsWith('#')) return node.id === selector.slice(1);
    if (selector.startsWith('.')) return node.classList.contains(selector.slice(1));
    const attr = selector.match(/^\[data-([\w-]+)(?:="([^"]+)")?\]$/u);
    if (attr) { const key = attr[1].replace(/-([a-z])/gu, (_, c) => c.toUpperCase()); return node.dataset[key] !== undefined && (attr[2] === undefined || node.dataset[key] === attr[2]); }
    return node.tag === selector;
  }
  function element(tag) {
    const classes = new Set();
    return {
      tag, dataset: {}, attrs: {}, style: {}, listeners: {}, children: [], value: '', hidden: false,
      classList: { add: name => classes.add(name), remove: name => classes.delete(name), contains: name => classes.has(name), toggle(name, yes = !classes.has(name)) { yes ? classes.add(name) : classes.delete(name); } },
      set className(value) { classes.clear(); value.split(/\s+/u).forEach(name => classes.add(name)); },
      setAttribute(name, value) { this.attrs[name] = String(value); },
      addEventListener: listen,
      append(...nodes) { for (const node of nodes) { if (node.parent) node.parent.children = node.parent.children.filter(child => child !== node); node.parent = this; this.children.push(node); } },
      replaceChildren(...nodes) { this.children.forEach(node => { node.parent = undefined; }); this.children = []; this.append(...nodes); },
      querySelector(selector) { return this.querySelectorAll(selector)[0]; },
      querySelectorAll(selector) { return walk(this.children).filter(node => matches(node, selector)); },
      closest(selector) { return matches(this, selector) ? this : this.parent?.closest(selector); },
      contains(node) { return walk([this]).includes(node); },
      focus() { document.activeElement = this; }, blur() { document.activeElement = document.body; },
      setPointerCapture(id) { this.capture = id; }, hasPointerCapture(id) { return this.capture === id; }, releasePointerCapture() { this.capture = undefined; },
      getBoundingClientRect() { return this.id === 'keyboard-languages' ? { left: 30, top: 620, bottom: 664, width: 44, height: 44 } : { left: 0, top: 0, width: 390, height: 740 }; },
      offsetWidth: 390, offsetHeight: 740, clientWidth: 390, clientHeight: 740, clientLeft: 0, clientTop: 0,
      getClientRects() { return this.hidden ? [] : [this.getBoundingClientRect()]; },
      scrollIntoView() {}, click() { fire(this, 'click'); },
    };
  }
  const tree = element('document'); const stack = [tree];
  const html = readFileSync(new URL('../index.html', import.meta.url), 'utf8').replace(/<!--[\s\S]*?-->/gu, '');
  for (const [, closing, tag, attributes] of html.matchAll(/<(\/)?([\w-]+)\b([^>]*)>/gu)) {
    if (closing) { if (stack.at(-1).tag === tag) stack.pop(); continue; }
    const node = element(tag);
    for (const [, name, value = ''] of attributes.matchAll(/([\w-]+)(?:="([^"]*)")?/gu)) {
      if (name === 'id') node.id = value;
      else if (name === 'class') node.className = value;
      else if (name.startsWith('data-')) node.dataset[name.slice(5).replace(/-([a-z])/gu, (_, c) => c.toUpperCase())] = value;
      else if (name === 'hidden') node.hidden = true;
      else node.setAttribute(name, value);
    }
    stack.at(-1).append(node);
    if (!['meta', 'link', 'input', 'path', 'br'].includes(tag)) stack.push(node);
  }
  const $ = selector => tree.querySelector(selector); const $$ = selector => tree.querySelectorAll(selector);
  document.body = $('body'); document.activeElement = document.body;
  function fire(target, name, properties = {}) {
    const event = { target, detail: 0, button: 0, pointerId: 7, clientX: 40, clientY: 640, defaultPrevented: false, preventDefault() { this.defaultPrevented = true; }, stopPropagation() { this.stopped = true; }, ...properties };
    const chain = [target]; let node = target.parent;
    while (node) { chain.push(node); node = node.parent; }
    if (target !== document && target !== window && target !== landscape) chain.push(document, window);
    for (const current of [...chain].reverse()) for (const { handler, capture } of current.listeners[name] ?? []) if (capture && !event.stopped) handler(event);
    for (const current of chain) for (const { handler, capture } of current.listeners[name] ?? []) if (!capture && !event.stopped) handler(event);
    return event;
  }
  const context = vm.createContext({ document, window, matchMedia: () => landscape, structuredClone });
  vm.runInContext(bundle, context);
  const ui = context.mobileUI.createMobileControls({
    root: $('#viewer'), keyboard: $('#keyboard'), storage: { getItem: key => store.get(key) ?? null, setItem: (key, value) => store.set(key, value) },
    active: () => connected,
    input: message => { if (connected) sent.push(JSON.parse(JSON.stringify(message))); },
    button: (bit, down) => { if (connected) sent.push({ bit, down }); },
    move: (dx, dy) => { if (connected) sent.push({ dx, dy }); },
    reset: () => sent.push({ kind: 'release' }), pointerMode: () => 'Trackpad',
  });
  sent.length = 0;
  return {
    $, $$, fire, ui, sent, store, document, window, landscape,
    click: node => fire(node, 'click'), point: node => { pointed = node; },
    hold() { for (const [id, fn] of [...timers]) { timers.delete(id); fn(); } },
    character(value) { const node = $$('[data-character]').find(key => key.dataset.character === value); assert.ok(node, `character ${value}`); fire(node, 'click'); },
    modifier(name) { return $$('.type-modifiers')[0].children.find(node => node.dataset.mod === name); },
    connected(value) { connected = value; },
  };
}
function type(a) { a.click(a.$('[data-mode="type"]')); a.sent.length = 0; }
function gesture(a, name, properties) { return a.fire(a.$('#keyboard-languages'), name, properties); }

test('real deck composes shifted text locally and sends one committed insertion', () => {
  const a = app(); type(a);
  a.click(a.modifier('Shift')); a.character('Q'); a.character('q');
  assert.equal(a.$('#compose').value, 'Qq'); assert.deepEqual(a.sent, []);
  assert.equal(a.modifier('Shift').attrs['aria-pressed'], 'false');
  a.click(a.$('#insert-text'));
  assert.deepEqual(a.sent, [{ kind: 'text', text: 'Qq' }]); assert.equal(a.$('#compose').value, '');
});

test('Ctrl/Alt chords consume modifiers without sending the local draft', () => {
  const a = app(); type(a); a.character('q');
  a.click(a.modifier('Ctrl')); a.click(a.modifier('Alt')); a.character('c');
  assert.deepEqual(a.sent.map(m => [m.keysym, m.down]), [[0xffe3, true], [0xffe9, true], [99, true], [99, false], [0xffe9, false], [0xffe3, false]]);
  assert.equal(a.$('#compose').value, 'q');
  a.sent.length = 0; a.click(a.modifier('Ctrl')); a.click(a.$('.dock-enter'));
  assert.equal(a.sent.some(m => m.kind === 'text'), false); assert.equal(a.$('#compose').value, 'q');
});

test('held Backspace repeats only while the custom Type keyboard remains active', () => {
  const a = app(); type(a); a.character('q'); a.character('w'); a.character('e');
  a.fire(a.$('#keyboard-backspace'), 'pointerdown');
  assert.equal(a.$('#compose').value, 'qw');
  a.hold(); assert.equal(a.$('#compose').value, 'q');
  a.click(a.$('[data-mode="keys"]')); a.hold();
  assert.equal(a.$('#compose').value, 'q');
  assert.equal(a.sent.some(m => m.kind === 'keysym'), false);
});

test('globe tap cycles once; hold slide release selects and preserves draft', () => {
  const a = app(); type(a); a.character('q');
  gesture(a, 'pointerdown'); gesture(a, 'pointerup'); a.fire(a.$('#keyboard-languages'), 'click', { detail: 1 });
  assert.equal(a.$('#compose').lang, 'th');
  gesture(a, 'pointerdown'); a.hold();
  const item = a.$$('[data-language-id]').find(n => n.dataset.languageId === 'ja');
  a.point(item); gesture(a, 'pointermove', { clientY: 400 }); gesture(a, 'pointerup', { clientY: 400 });
  assert.equal(a.$('#compose').lang, 'ja'); assert.equal(a.$('#compose').value, 'q');
  assert.equal(a.$('#language-popover').hidden, true);
  assert.equal(JSON.parse(a.store.get('topvnc.mobile.languages.v1')).selected, 'ja');
});

test('globe release-on-key, outside cancellation and keyboard navigation work', () => {
  const a = app(); type(a);
  gesture(a, 'pointerdown'); a.hold(); a.point(a.$('#keyboard-languages')); gesture(a, 'pointerup');
  assert.equal(a.$('#language-popover').hidden, false);
  a.fire(a.$('#language-popover'), 'keydown', { key: 'End' }); assert.equal(a.document.activeElement, a.$('[data-language-manage]'));
  a.fire(a.$('#language-popover'), 'keydown', { key: 'Escape' }); assert.equal(a.$('#language-popover').hidden, true);
  gesture(a, 'pointerdown'); a.hold(); a.point(a.$('#screen')); gesture(a, 'pointerup');
  assert.equal(a.$('#compose').lang, 'en'); assert.equal(a.$('#language-popover').hidden, true);
});

test('source, mode, orientation, cancellation and blur dismiss language menus', () => {
  for (const cancel of [
    a => a.click(a.$('[data-mode="keys"]')), a => a.click(a.$('#native-keyboard')),
    a => { a.landscape.matches = true; a.fire(a.landscape, 'change'); },
    a => a.fire(a.window, 'blur'), a => a.fire(a.window, 'pointercancel'),
    a => a.fire(a.$('#screen'), 'pointerdown'),
  ]) {
    const a = app(); type(a); gesture(a, 'pointerdown'); a.hold(); cancel(a);
    assert.equal(a.$('#language-popover').hidden, true);
  }
});

test('native composition blocks transitions and custom keys until commit', () => {
  const a = app(); type(a); a.click(a.$('#native-keyboard')); a.sent.length = 0;
  assert.equal(a.ui.nativeActive(), true); assert.equal(a.modifier('Ctrl').disabled, true);
  a.fire(a.$('#keyboard'), 'compositionstart');
  a.click(a.$('[data-mode="keys"]')); a.click(a.$('#custom-keyboard')); a.click(a.$('.dock-enter'));
  assert.equal(a.ui.nativeActive(), true); assert.deepEqual(a.sent, []);
  a.fire(a.$('#keyboard'), 'compositionend'); a.click(a.$('#custom-keyboard')); a.character('q');
  assert.equal(a.ui.nativeActive(), false); assert.equal(a.$('#compose').value, 'q');
});

test('native-only modifier disabling does not leak into Quick Keys', () => {
  const a = app(); type(a); a.click(a.$('#native-keyboard'));
  a.click(a.$('[data-mode="keys"]'));
  assert.ok(a.$$('[data-mod]').every(node => node.disabled === false));
  a.click(a.$('[data-mode="type"]'));
  assert.ok(a.$$('[data-mod]').every(node => node.disabled === true));
});

test('FPS pointers hold independently, cancel safely and route real look motion', () => {
  const saved = { preset: 'fps', fps: { bindings: { jump: 'W' } } };
  const a = app({ 'topvnc.mobile.customize.v1': JSON.stringify(saved) });
  a.click(a.$('[data-mode="pointer"]')); a.sent.length = 0;
  a.fire(a.$('[data-fps-action="forward"]'), 'pointerdown', { pointerId: 1 });
  a.fire(a.$('[data-fps-action="jump"]'), 'pointerdown', { pointerId: 2 });
  a.fire(a.$('[data-fps-action="fire"]'), 'pointerdown', { pointerId: 3 });
  a.fire(a.$('#fps-look'), 'pointerdown', { pointerId: 4, clientX: 10, clientY: 20 });
  a.fire(a.$('#fps-look'), 'pointermove', { pointerId: 4, clientX: 15, clientY: 17 });
  assert.deepEqual(a.sent, [{ kind: 'keysym', keysym: 119, down: true }, { bit: 1, down: true }, { dx: 5, dy: -3 }]);
  a.fire(a.$('[data-fps-action="forward"]'), 'pointerup', { pointerId: 1 });
  assert.equal(a.sent.length, 3);
  a.fire(a.$('[data-fps-action="jump"]'), 'lostpointercapture', { pointerId: 2 });
  assert.deepEqual(a.sent.at(-1), { kind: 'keysym', keysym: 119, down: false });
  a.ui.release(); assert.deepEqual(a.sent.at(-1), { bit: 1, down: false });
  a.ui.release(); assert.equal(a.sent.length, 5);
});

test('FPS mode/rotation/disconnect releases keys and does not replay disconnected input', () => {
  for (const exit of [a => a.click(a.$('[data-mode="type"]')), a => { a.landscape.matches = true; a.fire(a.landscape, 'change'); }, a => a.ui.closed()]) {
    const a = app({ 'topvnc.mobile.customize.v1': '{"preset":"fps"}' });
    a.click(a.$('[data-mode="pointer"]')); a.sent.length = 0;
    a.fire(a.$('[data-fps-action="forward"]'), 'pointerdown', { pointerId: 1 }); exit(a);
    assert.ok(a.sent.some(m => m.keysym === 119 && m.down === false));
    a.connected(false); a.sent.length = 0; a.character('q');
    assert.deepEqual(a.sent, []);
  }
});

test('Customize remains reachable with hidden custom keys and migrates legacy row order', () => {
  const a = app({ 'topvnc-wireframe-customize-v1': JSON.stringify({ layouts: { portrait: { groups: ['actions', 'input', 'languages'], showLanguages: false, showActions: false } } }) });
  type(a); assert.equal(a.$('.type-modifiers').hidden, false); assert.equal(a.$('.type-modifiers').style.order, '20');
  a.click(a.$('#customize-button'));
  assert.equal(a.$('#control-sheet').hidden, false); assert.equal(a.$('#control-deck').inert, true);
  a.click(a.$('#close-sheet')); assert.equal(a.$('#control-deck').inert, false);
});
