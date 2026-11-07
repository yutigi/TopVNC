import { createMobileInput } from './mobile-input.mjs';
import { createCustomization } from './customize.mjs';
import { createFpsController } from './fps.mjs';
import { createLanguageSwitcher } from './language-switcher.mjs';
import { readLanguages, normalizeLanguages, profileRows, validRows, THAI_SHIFT } from './languages.mjs';

export function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  if (tag === 'button') node.type = 'button';
  return node;
}

export function createMobileControls({ root, keyboard, storage, active, input, button, move, reset, pointerMode }) {
  const $ = selector => selector === '#device' ? root : root.querySelector(selector);
  const el = element;
  let mode = 'keys'; let source = 'custom'; let symbols = false; let composing = false;
  let languages = readLanguages(storage);
  const landscape = matchMedia('(orientation: landscape)');
  // A native keyboard may change the viewport aspect ratio without a rotation.
  // On touch devices prefer screen orientation, so opening the IME cannot cause
  // an orientation release that immediately blurs and closes that keyboard.
  const deviceOrientation = matchMedia('(pointer: coarse)').matches ? globalThis.screen?.orientation : undefined;
  const currentOrientation = () => (deviceOrientation?.type ? deviceOrientation.type.startsWith('landscape') : landscape.matches) ? 'landscape' : 'portrait';
  let orientation = currentOrientation();
  let customization; let switcher; let lastFocus;
  let eraseTimer; let erasing;
  let track; const pointerButtons = new Map();
  const pad = $('#trackpad'); const draft = $('#compose');
  const userAgent = globalThis.navigator?.userAgent ?? '';
  const os = /iPad|iPhone|iPod/u.test(userAgent) || (/Mac/u.test(userAgent) && globalThis.navigator?.maxTouchPoints > 1) ? 'iOS'
    : /Android/u.test(userAgent) ? 'Android' : /Windows/u.test(userAgent) ? 'Win' : /Mac/u.test(userAgent) ? 'Mac' : /Linux/u.test(userAgent) ? 'Linux' : 'OS';
  $('#native-keyboard').textContent = `Native · ${os}`;
  $('#native-keyboard').setAttribute('aria-label', `Request native ${os} keyboard input`);
  function feedback(value) { $('#control-status').textContent = value; }
  function canChange() { return !composing; }
  const sender = createMobileInput({ input, button, move, changed: () => renderKeyboard() });
  const fps = createFpsController({ $, el, input: sender, active });
  function persistLanguages() {
    languages = normalizeLanguages(languages);
    try {
      if (!storage) throw new Error('Storage unavailable');
      storage.setItem('topvnc.mobile.languages.v1', JSON.stringify(languages));
    } catch { feedback('Storage unavailable: language changes last for this session.'); }
  }
  function profile() { return languages.profiles.find(p => p.id === languages.selected) ?? languages.profiles[0]; }
  function release() {
    stopErase(); switcher?.cancel(); fps.release(); sender.release(); track = undefined;
    for (const bit of new Set(pointerButtons.values())) button(bit, false);
    pointerButtons.clear();
    composing = false;
  }
  function transition() { release(); reset(); }
  function setMode(next) {
    if (!canChange()) { feedback('Finish composing before changing mode.'); return; }
    transition(); mode = next;
    if (next !== 'type') keyboard.blur();
    for (const name of ['keys', 'type', 'pointer']) {
      $(`#${name}-panel`).hidden = name !== next;
      $(`[data-mode="${name}"]`).setAttribute('aria-pressed', String(name === next));
    }
    root.dataset.controlMode = mode;
    customization?.apply();
    renderKeyboard();
  }
  function selectLanguage(id) {
    if (!canChange() || !languages.profiles.some(p => p.id === id)) return;
    sender.clearModifiers(); languages.selected = id; persistLanguages(); renderKeyboard();
  }
  function press(key) {
    if (!active() || composing) return;
    sender.tap(key);
  }
  function addCharacter(value) {
    if (!active() || composing || source !== 'custom') return;
    if (sender.modifiers.has('Ctrl') || sender.modifiers.has('Alt') || sender.modifiers.has('Win')) {
      // Chords use one character; multi-character custom keys remain text only.
      if ([...value].length === 1) press(value === ' ' ? 'Space' : value);
      else feedback('Choose a single character for a modifier chord.');
      return;
    }
    if (draft.value.length + value.length <= 2000) draft.value += value;
    else feedback('Insert or clear the draft before adding more text (2000 characters).');
    sender.clearModifiers();
  }
  function insert() {
    if (!active() || composing) return;
    if (sender.modifiers.size) { press('Enter'); return; }
    if (mode === 'type' && source === 'custom' && draft.value) {
      if (sender.text(draft.value)) draft.value = '';
    } else press('Enter');
  }
  function renderKeyboard() {
    const shifted = sender.modifiers.has('Shift');
    root.querySelectorAll('[data-mod]').forEach(node => {
      node.setAttribute('aria-pressed', String(sender.modifiers.has(node.dataset.mod)));
      node.disabled = source === 'native' && mode === 'type';
    });
    const rows = symbols ? ['1 2 3 4 5 6 7 8 9 0', '@ # $ % & * ( ) /', '- _ + = : ; ! ? .'].map(row => row.split(' ')) : profileRows(profile());
    $('#keyboard-rows').replaceChildren();
    rows.forEach((keys, index) => {
      const row = el('div', undefined, 'keyboard-row');
      keys.forEach((character, column) => {
        const value = shifted && !symbols ? (profile().id === 'th' && !profile().rows ? THAI_SHIFT[index][column] : character.toLocaleUpperCase()) : character;
        const key = el('button', value, 'character-key'); key.dataset.character = value;
        if (column === Math.ceil(keys.length / 2)) key.classList.add('split-start');
        key.addEventListener('click', () => addCharacter(value)); row.append(key);
      });
      $('#keyboard-rows').append(row);
    });
    $('#symbol-keyboard').textContent = symbols ? 'ABC' : '?123';
    $('#symbol-keyboard').setAttribute('aria-pressed', String(symbols));
    $('#keyboard-space').textContent = profile().name;
    $('#keyboard-languages').setAttribute('aria-label', `Keyboard language: ${profile().name}. Tap to cycle; hold to choose.`);
    draft.lang = profile().tag;
    $('#typing-help').textContent = source === 'native'
      ? 'Native input sends committed text directly. Use your OS globe for languages; your OS controls keyboard visibility.'
      : ['zh', 'ja'].includes(profile().id)
        ? 'Direct character layout. For Chinese/Japanese conversion, use Native input and your device IME.'
        : 'Compose locally, then Insert. Ctrl/Alt shortcuts send the next key directly.';
  }
  function setSource(next) {
    if (!canChange()) { feedback('Finish composing before changing input source.'); return; }
    transition(); source = next;
    $('#custom-keyboard-panel').hidden = next !== 'custom';
    $('#local-draft-panel').hidden = next !== 'custom';
    $('#native-input-panel').hidden = next !== 'native';
    for (const name of ['custom', 'native']) $(`#${name}-keyboard`).setAttribute('aria-pressed', String(next === name));
    renderKeyboard();
    if (next === 'native') keyboard.focus(); else keyboard.blur();
  }
  function closeSheet() {
    $('#control-sheet').hidden = true;
    for (const id of ['screen', 'toolbar', 'control-deck', 'menu']) $(`#${id}`).inert = false;
    lastFocus?.focus();
  }
  function openSheet(title, build) {
    if (!canChange()) { feedback('Finish composing before opening settings.'); return; }
    lastFocus = document.activeElement; transition(); keyboard.blur();
    $('#sheet-title').textContent = title; build($('#sheet-content'));
    $('#control-sheet').hidden = false;
    for (const id of ['screen', 'toolbar', 'control-deck', 'menu']) $(`#${id}`).inert = true;
    $('#close-sheet').focus();
  }
  function buildLanguages(content, message = '') {
    content.replaceChildren(el('p', message || 'Favorite up to four profiles. Edit three character rows or add another language.'));
    for (const p of languages.profiles) {
      const row = el('div', undefined, 'profile-row');
      const choose = el('button', p.name); choose.setAttribute('aria-pressed', String(p.id === languages.selected));
      choose.addEventListener('click', () => { selectLanguage(p.id); closeSheet(); });
      const pin = el('button', p.pinned ? '★' : '☆'); pin.setAttribute('aria-label', `Favorite ${p.name}`); pin.setAttribute('aria-pressed', String(p.pinned));
      pin.addEventListener('click', () => {
        if (!p.pinned && languages.profiles.filter(item => item.pinned).length >= 4) { buildLanguages(content, 'Unfavorite a profile before adding a fifth favorite.'); return; }
        p.pinned = !p.pinned; persistLanguages(); buildLanguages(content);
      });
      const edit = el('button', 'Edit'); edit.setAttribute('aria-label', `Edit ${p.name}`);
      edit.addEventListener('click', () => buildLayout(content, p.id));
      const remove = el('button', 'Remove'); remove.setAttribute('aria-label', `Remove ${p.name}`); remove.disabled = languages.profiles.length === 1;
      remove.addEventListener('click', () => {
        languages.profiles = languages.profiles.filter(item => item.id !== p.id);
        persistLanguages(); renderKeyboard(); buildLanguages(content);
      });
      row.append(choose, pin, edit, remove); content.append(row);
    }
    const form = el('form', undefined, 'custom-key-form');
    const label = el('label', 'Language name'); const name = el('input'); name.maxLength = 40; name.required = true; label.append(name);
    const add = el('button', 'Add language'); add.type = 'submit'; form.append(label, add);
    form.addEventListener('submit', event => {
      event.preventDefault();
      if (!name.value.trim() || languages.profiles.length >= 16) { buildLanguages(content, 'Use a name; up to 16 profiles are allowed.'); return; }
      languages.profiles.push({ id: `custom-${Date.now()}`, name: name.value.trim(), label: name.value.trim(), tag: 'und', pinned: languages.profiles.filter(p => p.pinned).length < 4 });
      persistLanguages(); buildLanguages(content);
    });
    content.append(form, el('p', 'These layouts do not change your operating system keyboard. Native input provides device IME conversion.'));
  }
  function buildLayout(content, id) {
    const p = languages.profiles.find(item => item.id === id);
    content.replaceChildren(el('p', `${p.name}: three rows, with spaces between keys (3–14 keys per row, up to 8 characters per key).`));
    const field = el('textarea'); field.rows = 4; field.maxLength = 500; field.value = profileRows(p).map(row => row.join(' ')).join('\n');
    field.setAttribute('aria-label', `${p.name} keyboard rows`);
    const status = el('p'); status.setAttribute('role', 'status');
    const save = el('button', 'Save keyboard layout');
    save.addEventListener('click', () => {
      const rows = field.value.trim().split(/\r?\n/u).map(row => row.trim().split(/\s+/u));
      if (!validRows(rows)) { status.textContent = 'Use three rows of 3–14 keys, at most 8 characters per key.'; return; }
      p.rows = rows; persistLanguages(); renderKeyboard(); buildLanguages(content, 'Keyboard layout saved.');
    });
    const back = el('button', 'Back to languages'); back.addEventListener('click', () => buildLanguages(content));
    content.append(field, status, save, back); field.focus();
  }
  customization = createCustomization({
    $, el, storage, orientation: () => orientation, mode: () => mode, canChange,
    key: press, text: value => { if (active() && !composing) { transition(); sender.text(value); } },
    close: closeSheet, feedback, fps: fps.apply,
    enterPreset: () => setMode('pointer'),
  });
  switcher = createLanguageSwitcher({
    key: $('#keyboard-languages'), host: root, el,
    profiles: () => languages.profiles, current: () => languages.selected,
    choose: selectLanguage, configure: () => openSheet('Keyboard languages', buildLanguages),
    canChange: () => !composing && mode === 'type' && source === 'custom', beforeOpen: () => { stopErase(); sender.clearModifiers(); }, feedback,
  });
  root.querySelectorAll('[data-mode]').forEach(node => node.addEventListener('click', () => setMode(node.dataset.mode)));
  root.addEventListener('click', event => {
    const modifier = event.target.closest('[data-mod]');
    if (modifier && active() && !composing && !(mode === 'type' && source === 'native')) sender.toggle(modifier.dataset.mod);
    const key = event.target.closest('[data-mobile-key]'); if (key) press(key.dataset.mobileKey);
  });
  // Retain draft/native focus while tapping custom controls, without blocking
  // inputs in the customization dialog or buttons' keyboard activation.
  $('#control-deck').addEventListener('pointerdown', event => { if (event.target.closest('button')) event.preventDefault(); });
  $('.dock-enter').addEventListener('click', insert);
  $('#insert-text').addEventListener('click', insert);
  $('#keyboard-space').addEventListener('click', () => addCharacter(' '));
  function backspace() {
    if (!active() || composing) return;
    if (sender.modifiers.size) press('Backspace');
    else if (draft.value) draft.value = [...draft.value].slice(0, -1).join('');
    else press('Backspace');
  }
  function stopErase() { window.clearTimeout(eraseTimer); eraseTimer = undefined; erasing = undefined; }
  function repeatErase() {
    if (erasing === undefined || !active() || mode !== 'type' || source !== 'custom' || composing) { stopErase(); return; }
    backspace(); eraseTimer = window.setTimeout(repeatErase, 70);
  }
  const eraseKey = $('#keyboard-backspace');
  eraseKey.addEventListener('pointerdown', event => {
    if (!active() || composing || source !== 'custom') return;
    event.preventDefault(); stopErase(); eraseKey.setPointerCapture(event.pointerId);
    const chord = sender.modifiers.size > 0; backspace();
    if (!chord) { erasing = event.pointerId; eraseTimer = window.setTimeout(repeatErase, 350); }
  });
  eraseKey.addEventListener('click', event => { if (event.detail === 0) backspace(); });
  for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) eraseKey.addEventListener(name, event => { if (erasing === event.pointerId) stopErase(); });
  $('#clear-draft').addEventListener('click', () => { if (!composing) draft.value = ''; });
  $('#symbol-keyboard').addEventListener('click', () => { symbols = !symbols; renderKeyboard(); });
  $('#custom-keyboard').addEventListener('click', () => setSource('custom'));
  $('#native-keyboard').addEventListener('click', () => setSource('native'));
  $('#show-keyboard').addEventListener('click', () => { setMode('type'); });
  $('#customize-button').addEventListener('click', () => openSheet('Customize keys and layout', customization.build));
  $('#keyboard-layout').addEventListener('click', () => openSheet('Keyboard languages', buildLanguages));
  $('#close-sheet').addEventListener('click', closeSheet);
  $('#control-sheet').addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.preventDefault(); closeSheet(); }
    if (event.key === 'Tab') {
      const nodes = [...$('#control-sheet').querySelectorAll('button, input, textarea, select')].filter(node => !node.disabled && node.getClientRects().length);
      const first = nodes[0]; const last = nodes.at(-1);
      if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last?.focus(); }
      else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first?.focus(); }
    }
  });
  keyboard.addEventListener('compositionstart', () => { release(); composing = true; });
  keyboard.addEventListener('compositionend', () => { composing = false; });
  // Standard Pointer deck: a separate relative trackpad and independent buttons.
  pad.addEventListener('pointerdown', event => {
    if (!active() || track) return;
    event.preventDefault(); pad.setPointerCapture(event.pointerId); track = { id: event.pointerId, x: event.clientX, y: event.clientY };
  });
  pad.addEventListener('pointermove', event => {
    if (!active() || track?.id !== event.pointerId) return;
    move(event.clientX - track.x, event.clientY - track.y); track.x = event.clientX; track.y = event.clientY;
  });
  for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) pad.addEventListener(name, event => { if (track?.id === event.pointerId) track = undefined; });
  root.querySelectorAll('[data-pointer-button]').forEach(node => {
    node.addEventListener('pointerdown', event => {
      if (!active() || pointerButtons.size >= 16) return;
      event.preventDefault(); node.setPointerCapture(event.pointerId); const bit = Number(node.dataset.pointerButton);
      if (![...pointerButtons.values()].includes(bit)) button(bit, true); pointerButtons.set(event.pointerId, bit);
    });
    for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) node.addEventListener(name, event => {
      const bit = pointerButtons.get(event.pointerId); pointerButtons.delete(event.pointerId);
      if (bit && ![...pointerButtons.values()].includes(bit)) button(bit, false);
    });
    node.addEventListener('click', event => {
      if (event.detail === 0 && active()) { button(Number(node.dataset.pointerButton), true); button(Number(node.dataset.pointerButton), false); }
    });
  });
  $('#pointer-mode-label').textContent = pointerMode();
  function rotate() {
    const next = currentOrientation();
    if (next === orientation) return;
    transition(); orientation = next; customization.apply();
  }
  (deviceOrientation ?? landscape).addEventListener('change', rotate);
  setMode('keys'); setSource('custom');
  return {
    release, tap: press, clearModifiers: sender.clearModifiers,
    beginPhysical: sender.beginPhysical, endPhysical: sender.endPhysical,
    nativeActive: () => source === 'native' && mode === 'type',
    closed() { release(); composing = false; draft.value = ''; closeSheet(); },
  };
}
