import { createCustomization } from './customize.mjs';
const $ = (selector) => document.querySelector(selector);
const $$ = (selector) => [...document.querySelectorAll(selector)];
const defaults = [
  { id: 'en', label: 'EN', name: 'English', tag: 'en' },
  { id: 'th', label: 'ไทย', name: 'Thai', tag: 'th' },
  { id: 'zh', label: '中文', name: 'Chinese', tag: 'zh' },
  { id: 'ja', label: '日本語', name: 'Japanese', tag: 'ja' },
];
const shortcuts = ['Ctrl+C', 'Ctrl+V', 'Ctrl+Z', 'Ctrl+A', 'Win', 'Fn', 'Space', 'Delete', 'Home', 'End', 'Escape'];
let profiles = defaults.map((p) => ({ ...p, pinned: true }));
let pins = ['Ctrl+C', 'Ctrl+V', 'Win', 'Fn'];
let selected = 'en';
try {
  const saved = JSON.parse(localStorage.getItem('topvnc-wireframe-v1'));
  if (saved && Array.isArray(saved.profiles) && saved.profiles.length > 0 && saved.profiles.length <= 16 && saved.profiles.every(p => typeof p.id === 'string' && typeof p.name === 'string' && p.name.length <= 40 && typeof p.label === 'string' && p.label.length <= 40 && typeof p.tag === 'string' && typeof p.pinned === 'boolean') && new Set(saved.profiles.map(p => p.id)).size === saved.profiles.length && saved.profiles.filter(p => p.pinned).length <= 4) {
    profiles = saved.profiles;
    selected = profiles.some(p => p.id === saved.selected) ? saved.selected : profiles[0].id;
  }
  if (Array.isArray(saved?.pins) && saved.pins.length === 4 && saved.pins.every(p => shortcuts.includes(p))) pins = saved.pins;
} catch { /* Storage can be unavailable in private browser contexts. */ }
let mode = 'type';
let keyboardSource = 'custom';
let keyboardShift = false;
let symbols = false;
const latinRows = ['q w e r t y u i o p', 'a s d f g h j k l', 'z x c v b n m'];
const thaiRows = ['ๆ ไ ำ พ ะ ั ี ร น ย บ ล', 'ฟ ห ก ด เ ้ ่ า ส ว ง', 'ผ ป แ อ ิ ื ท ม ใ ฝ'];
const symbolRows = ['1 2 3 4 5 6 7 8 9 0', '@ # $ % & * ( ) /', '- _ + = : ; ! ? .'];
let orientation = 'portrait';
let composing = false;
let focusMode = false;
let lastFocus;
let sheetKind;
let customization;
const modifiers = new Set();
function save() {
  try { localStorage.setItem('topvnc-wireframe-v1', JSON.stringify({ profiles, pins, selected })); } catch { /* Session-only preferences still work. */ }
}
function el(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}
function feedback(text) { $('#key-feedback').textContent = text; $('#session-feedback').textContent = text; }
function release() {
  modifiers.clear();
  keyboardShift = false;
  stopErase();
  renderKeyboard();
  $$('[data-mod]').forEach(b => b.setAttribute('aria-pressed', 'false'));
}
function currentProfile() { return profiles.find(p => p.id === selected) ?? profiles[0]; }
function renderLanguages() {
  const container = $('#language-pins');
  container.replaceChildren();
  profiles.filter(p => p.pinned).forEach(profile => {
    const button = el('button', profile.label, 'language-chip');
    button.lang = profile.tag;
    button.setAttribute('aria-label', `${profile.name} typing profile`);
    button.setAttribute('aria-pressed', String(selected === profile.id));
    button.addEventListener('click', () => selectLanguage(profile.id));
    container.append(button);
  });
  const profile = currentProfile();
  $('#language-hint').textContent = `${profile.name} · ${keyboardSource === 'custom' ? 'TopVNC on-screen layout' : 'native OS input'}`;
  $('#compose').lang = profile.tag;
  $('#ime-example').textContent = keyboardSource === 'native' ? 'Native input: use the OS globe key to switch languages. Keyboard visibility is controlled by your OS.' : profile.id === 'zh' ? 'Demo Pinyin: try nihao or zhongwen. A full custom IME is future implementation.' : profile.id === 'ja' ? 'Demo romaji: try nihon or arigatou. A full custom IME is future implementation.' : 'TopVNC keys · edit the layout to match your habits.';
  renderKeyboard();
  renderCandidates();
}
function selectLanguage(id) {
  if (composing) { feedback('Finish composing before changing profile'); return; }
  selected = id;
  renderLanguages();
  setMode('type');
  feedback(`${currentProfile().name} profile selected`);
  save();
}
function renderPins() {
  $('#pin-row').replaceChildren();
  pins.forEach((key, i) => {
    const button = el('button', key);
    button.dataset.key = key;
    button.setAttribute('aria-label', `Pinned slot ${i + 1}: ${key}`);
    $('#pin-row').append(button);
  });
}
function setMode(next) {
  if (composing && next !== 'type') { feedback('Finish composing before changing mode'); return; }
  release();
  mode = next;
  $('#device').classList.toggle('typing', next === 'type');
  ['keys', 'type', 'pointer'].forEach(value => { $(`#${value}-panel`).hidden = value !== next; });
  $$('[data-mode]').forEach(b => { b.classList.toggle('active', b.dataset.mode === next); b.setAttribute('aria-pressed', String(b.dataset.mode === next)); });
  $('#deck-label').textContent = { keys: 'QUICK KEYS', type: 'MULTILINGUAL TYPING', pointer: 'TRACKPAD' }[next];
  $('#remote-hint-text').textContent = next === 'type' ? 'Compose below · insert when ready' : 'Drag to move · tap to click';
  updateCaption();
  customization?.apply(orientation);
}
function updateCaption() {
  $('#preview-caption').textContent = orientation === 'portrait' ? 'The remote view stays above a fixed control deck.' : mode === 'type' ? 'Split key rows below the view. The dock keeps its order.' : 'The same dock order, alongside your remote view.';
}
function setOrientation(value) {
  release();
  orientation = value;
  $('#device').classList.toggle('portrait', value === 'portrait');
  $('#device').classList.toggle('landscape', value === 'landscape');
  $$('[data-view]').forEach(b => { b.classList.toggle('selected', b.dataset.view === value); b.setAttribute('aria-pressed', String(b.dataset.view === value)); });
  $('#canvas-title').textContent = value === 'portrait' ? 'Portrait / two-thumb reach' : 'Landscape / room to play';
  $('#dimension-label').textContent = value === 'portrait' ? '390 × 740 / logical preview' : '844 × 414 / logical preview';
  updateCaption();
  customization?.apply(orientation);
  feedback('View rotated · modifiers released');
}
function pressKey(key, button) {
  if (key === 'Fn') { release(); openSheet('fn', button); return; }
  if (key === 'Enter' && mode === 'type' && $('#compose').value) { insertText(); return; }
  const combined = [...new Set([...modifiers, ...key.split('+')])].join(' + ');
  feedback(`${combined} · demo press`);
  release();
  if (button) {
    button.classList.remove('press-flash');
    void button.offsetWidth;
    button.classList.add('press-flash');
  }
}
document.addEventListener('click', event => {
  const key = event.target.closest('[data-key]');
  const modifier = event.target.closest('[data-mod]');
  const changeMode = event.target.closest('[data-mode]');
  if (key) pressKey(key.dataset.key, key);
  if (modifier) {
    const value = modifier.dataset.mod;
    modifiers.has(value) ? modifiers.delete(value) : modifiers.add(value);
    modifier.setAttribute('aria-pressed', String(modifiers.has(value)));
    feedback(modifiers.size ? `${[...modifiers].join(' + ')} · next key` : 'Modifiers released');
  }
  if (changeMode) setMode(changeMode.dataset.mode);
});
function insertText() {
  if (composing) { feedback('Confirm your IME composition first'); return; }
  const value = $('#compose').value;
  if (!value.trim()) { feedback('Type something to insert'); $('#compose').focus(); return; }
  $('#remote-output').textContent = value.slice(0, 2000);
  $('#compose').value = '';
  feedback('Text inserted in demo screen');
  $('#composition-status').textContent = 'Inserted · ready for the next phrase';
  renderCandidates();
}
$('#compose').addEventListener('compositionstart', () => {
  composing = true;
  $('#insert-text').disabled = true;
  $('#composition-status').textContent = 'Composing… choose characters first';
});
$('#compose').addEventListener('compositionend', () => {
  composing = false;
  $('#insert-text').disabled = false;
  $('#composition-status').textContent = 'Ready to insert';
});
$('#compose').addEventListener('input', () => { if (!composing) $('#composition-status').textContent = $('#compose').value ? 'Ready to insert' : 'Text stays here until Insert'; renderCandidates(); });
$('#insert-text').addEventListener('click', insertText);
$('#native-keyboard').addEventListener('click', () => setKeyboardSource('native'));
$('#custom-keyboard').addEventListener('click', () => setKeyboardSource('custom'));
function closeSheet() {
  $('#sheet').hidden = true;
  $('#sheet').removeAttribute('role');
  $('#sheet').removeAttribute('aria-modal');
  $('.session-bar').inert = false;
  $('.session-body').inert = false;
  sheetKind = undefined;
  if (lastFocus?.isConnected) lastFocus.focus();
}
function openSheet(kind, trigger = document.activeElement) {
  release();
  if ($('#sheet').hidden) lastFocus = trigger;
  sheetKind = kind;
  const content = $('#sheet-content');
  content.replaceChildren();
  $('#sheet-title').textContent = { customize: 'Make it yours', languages: 'Your languages', layout: 'Edit keyboard layout', pins: 'Your pinned keys', settings: 'Session settings', host: 'Your machines', fn: 'Function keys' }[kind];
  if (kind === 'customize') customization.build(content);
  if (kind === 'languages') buildLanguages(content);
  if (kind === 'pins') buildPins(content);
  if (kind === 'layout') buildLayout(content);
  if (kind === 'settings') {
    content.append(el('p', 'Wireframe session. No live host is connected.', 'sheet-description'));
    [['Machine', 'Gaming PC · demo'], ['Security', 'Simulated · not connected'], ['Transport design', 'WebSocket gateway'], ['Input profile', currentProfile().name]].forEach(([key, value]) => {
      const row = el('div', undefined, 'setting-row'); row.append(el('span', key), el('span', value)); content.append(row);
    });
    const languages = el('button', 'Configure languages →', 'wide-button');
    languages.addEventListener('click', () => openSheet('languages'));
    const pinButton = el('button', 'Edit pinned shortcuts →', 'wide-button');
    pinButton.addEventListener('click', () => openSheet('pins'));
    const customizeButton = el('button', 'Customize keys & UI layout →', 'wide-button');
    customizeButton.addEventListener('click', () => openSheet('customize'));
    content.append(customizeButton, languages, pinButton, el('p', 'Production design: show authentication and encryption state here. Never silently connect without authentication.', 'sheet-notice'));
  }
  if (kind === 'host') {
    content.append(el('p', 'Machine picker placement. These are sample machines only.', 'sheet-description'));
    ['Gaming PC · selected', 'Workstation · sample'].forEach(name => { const b = el('button', name, 'wide-button'); b.addEventListener('click', () => { feedback('Machine switching is a design placeholder'); closeSheet(); }); content.append(b); });
    content.append(el('p', 'Switching machines in the real client must release held keys and disconnect the current session first.', 'sheet-notice'));
  }
  if (kind === 'fn') {
    content.append(el('p', 'A fixed grid for the keys you use less often.', 'sheet-description'));
    const grid = el('div', undefined, 'function-grid');
    [...Array.from({ length: 12 }, (_, i) => `F${i + 1}`), 'Home', 'End', 'PgUp', 'PgDn'].forEach(key => { const b = el('button', key); b.dataset.key = key; grid.append(b); });
    content.append(grid);
  }
  $('#sheet').hidden = false;
  $('#sheet').setAttribute('role', 'dialog');
  $('#sheet').setAttribute('aria-modal', 'true');
  $('#sheet').setAttribute('aria-labelledby', 'sheet-title');
  $('.session-bar').inert = true;
  $('.session-body').inert = true;
  $('#close-sheet').focus();
}
function buildLanguages(content, message = '') {
  content.replaceChildren(el('p', 'Pin up to four keyboard profiles for one-tap access. Edit their key layouts or add your own.', 'sheet-description'));
  if (message) { const status = el('p', message, 'sheet-description'); status.setAttribute('role', 'status'); content.append(status); }
  profiles.forEach(profile => {
    const row = el('div', undefined, 'language-option');
    const choose = el('button', profile.name);
    choose.className = 'profile-choose';
    choose.setAttribute('aria-label', `Use ${profile.name}`);
    choose.addEventListener('click', () => { if (composing) { buildLanguages(content, 'Finish composing before changing profile.'); return; } closeSheet(); selectLanguage(profile.id); });
    const pin = el('button', profile.pinned ? 'Pinned' : 'Pin');
    pin.setAttribute('aria-label', `${profile.pinned ? 'Unpin' : 'Pin'} ${profile.name}`);
    pin.setAttribute('aria-pressed', String(profile.pinned));
    pin.addEventListener('click', () => {
      if (!profile.pinned && profiles.filter(p => p.pinned).length >= 4) { buildLanguages(content, 'Four profiles are pinned. Unpin one to make room.'); return; }
      profile.pinned = !profile.pinned; save(); renderLanguages(); buildLanguages(content);
    });
    const edit = el('button', 'Edit');
    edit.setAttribute('aria-label', `Edit ${profile.name} keyboard layout`);
    edit.addEventListener('click', () => { if (composing) { buildLanguages(content, 'Finish composition first.'); return; } selected = profile.id; renderLanguages(); openSheet('layout'); });
    const remove = el('button', '×');
    remove.setAttribute('aria-label', `Remove ${profile.name}`);
    remove.disabled = profiles.length === 1;
    remove.addEventListener('click', () => {
      if (composing) { buildLanguages(content, 'Finish composing before removing a language.'); return; }
      profiles = profiles.filter(p => p.id !== profile.id);
      if (selected === profile.id) selected = profiles[0].id;
      save(); renderLanguages(); buildLanguages(content);
    });
    row.append(choose, pin, edit, remove); content.append(row);
  });
  const form = el('form', undefined, 'add-language');
  const field = el('input'); field.placeholder = 'Add a language or layout'; field.maxLength = 40; field.required = true; field.setAttribute('aria-label', 'Language or layout name');
  const add = el('button', 'Add'); add.type = 'submit'; form.append(field, add);
  form.addEventListener('submit', event => {
    event.preventDefault();
    const name = field.value.trim();
    if (!name) return;
    if (profiles.length >= 16) { buildLanguages(content, 'Up to 16 profiles are available in this wireframe.'); return; }
    if (profiles.some(p => p.name.toLowerCase() === name.toLowerCase())) { buildLanguages(content, 'That language is already in your list.'); return; }
    profiles.push({ id: `custom-${Date.now()}`, name, label: name, tag: 'und', pinned: profiles.filter(p => p.pinned).length < 4 });
    save(); renderLanguages(); buildLanguages(content, `${name} added. Pin it for direct access.`);
  });
  content.append(form, el('p', 'TopVNC keys are independent of your OS keyboard. Custom profiles start with editable QWERTY rows. Chinese/Japanese candidate examples are simulated; use native input for a complete device IME.', 'sheet-notice'));
}
function buildPins(content) {
  content.append(el('p', 'Four fixed slots. Choose each shortcut yourself; they never reorder automatically.', 'sheet-description'));
  const group = el('div', undefined, 'pin-editor');
  const fields = pins.map((value, index) => {
    const label = el('label', `Slot ${index + 1}`);
    const select = el('select');
    shortcuts.forEach(key => { const option = el('option', key); option.value = key; option.selected = key === value; select.append(option); });
    label.append(select); group.append(label); return select;
  });
  const button = el('button', 'Save pinned keys', 'wide-button');
  button.addEventListener('click', () => { pins = fields.map(field => field.value); save(); renderPins(); closeSheet(); feedback('Pinned positions saved'); });
  content.append(group, button);
}
$('#close-sheet').addEventListener('click', closeSheet);
$('#language-settings').addEventListener('click', () => openSheet('languages'));
$('#inspector-languages').addEventListener('click', () => openSheet('languages'));
$('#settings-button').addEventListener('click', () => openSheet('settings'));
$('#customize-button').addEventListener('click', () => openSheet('customize'));
$('#host-button').addEventListener('click', () => openSheet('host'));
$('#edit-pins').addEventListener('click', () => openSheet('pins'));
$$('[data-view]').forEach(b => b.addEventListener('click', () => setOrientation(b.dataset.view)));
$('#try-rotate').addEventListener('click', () => setOrientation(orientation === 'portrait' ? 'landscape' : 'portrait'));
$('#try-language').addEventListener('click', () => openSheet('languages'));
$('#try-shortcut').addEventListener('click', () => { setMode('keys'); pressKey('Ctrl+C', $('#pin-row button')); });
function setFocusPreview(active) {
  focusMode = active;
  document.body.classList.toggle('focus-mode', active);
  $('#exit-focus').hidden = !active;
  if (active) { setOrientation(innerWidth > innerHeight ? 'landscape' : 'portrait'); $('#exit-focus').focus(); }
  else { $('#focus-preview').focus(); }
}
$('#focus-preview').addEventListener('click', () => setFocusPreview(true));
$('#exit-focus').addEventListener('click', () => setFocusPreview(false));
window.addEventListener('resize', () => { if (focusMode && document.activeElement !== $('#compose')) setOrientation(innerWidth > innerHeight ? 'landscape' : 'portrait'); });
window.addEventListener('blur', release);
document.addEventListener('visibilitychange', release);
window.addEventListener('pointercancel', release);
window.addEventListener('pagehide', release);
document.addEventListener('keydown', event => {
  if (event.key === 'Escape') { if (!$('#sheet').hidden) closeSheet(); else if (focusMode) setFocusPreview(false); release(); }
  if (event.key === 'Tab' && !$('#sheet').hidden) {
    const targets = [...$('#sheet').querySelectorAll('button:not(:disabled),input,select,textarea')];
    const first = targets[0]; const last = targets.at(-1);
    if (event.shiftKey && document.activeElement === first) { event.preventDefault(); last.focus(); }
    else if (!event.shiftKey && document.activeElement === last) { event.preventDefault(); first.focus(); }
  }
});
let drag;
$('#trackpad').addEventListener('pointerdown', event => {
  drag = { x: event.clientX, y: event.clientY, moved: false };
  $('#trackpad').setPointerCapture(event.pointerId);
});
$('#trackpad').addEventListener('pointermove', event => {
  if (!drag) return;
  const dx = event.clientX - drag.x; const dy = event.clientY - drag.y;
  if (Math.abs(dx) + Math.abs(dy) > 3) drag.moved = true;
  const screen = $('#remote'); const cursor = $('.remote-cursor');
  const x = parseFloat(cursor.style.left) || 65; const y = parseFloat(cursor.style.top) || 66;
  cursor.style.left = `${Math.min(93, Math.max(3, x + dx / screen.clientWidth * 100))}%`;
  cursor.style.top = `${Math.min(85, Math.max(8, y + dy / screen.clientHeight * 100))}%`;
  drag.x = event.clientX; drag.y = event.clientY;
});
$('#trackpad').addEventListener('pointerup', () => { if (drag && !drag.moved) feedback('Left click · demo press'); drag = undefined; });
$('#trackpad').addEventListener('pointercancel', () => { drag = undefined; });
function profileRows(profile = currentProfile()) {
  if (Array.isArray(profile.rows) && profile.rows.length === 3 && profile.rows.every(row => Array.isArray(row) && row.length > 0 && row.length <= 14 && row.every(key => typeof key === 'string' && key.length <= 8))) return profile.rows;
  return (profile.id === 'th' ? thaiRows : latinRows).map(row => row.split(' '));
}
function renderKeyboard() {
  const rows = symbols ? symbolRows.map(row => row.split(' ')) : profileRows();
  const thaiShift = ['๐ " ฎ ฑ ธ ํ ๊ ณ ฯ ญ ฐ ,', 'ฤ ฆ ฏ โ ฌ ็ ๋ ษ ศ ซ .', '( ) ฉ ฮ ฺ ์ ? ฒ ฬ ฦ'].map(row => row.split(' '));
  $('#keyboard-rows').replaceChildren();
  rows.forEach((keys, rowIndex) => {
    const row = el('div', undefined, 'keyboard-row');
    if (rowIndex === 2) {
      const shift = el('button', '⇧', 'keyboard-shift');
      shift.setAttribute('aria-label', 'Shift'); shift.setAttribute('aria-pressed', String(keyboardShift));
      shift.addEventListener('click', () => { keyboardShift = !keyboardShift; renderKeyboard(); });
      row.append(shift);
    }
    keys.forEach((character, index) => {
      const displayed = keyboardShift && !symbols ? (currentProfile().id === 'th' && !currentProfile().rows ? thaiShift[rowIndex][index] : character.toLocaleUpperCase()) : character;
      const button = el('button', displayed, 'character-key');
      button.setAttribute('aria-label', displayed);
      button.dataset.character = displayed;
      if (index === Math.ceil(keys.length / 2)) button.classList.add('split-start');
      row.append(button);
    });
    $('#keyboard-rows').append(row);
  });
  $('#symbol-keyboard').textContent = symbols ? 'ABC' : '?123';
  $('#symbol-keyboard').setAttribute('aria-pressed', String(symbols));
  $('#keyboard-space').textContent = currentProfile().name;
}
function addCharacter(value) {
  if (composing || keyboardSource !== 'custom') return;
  const field = $('#compose');
  if (field.value.length + value.length > 2000) { feedback('Demo text limit reached'); return; }
  field.value += value;
  if (keyboardShift) { keyboardShift = false; renderKeyboard(); }
  $('#composition-status').textContent = 'Composed locally · tap Insert when ready';
  feedback(value === ' ' ? 'Space · custom key' : `${value} · custom key`);
  renderCandidates();
}
document.addEventListener('pointerdown', event => {
  const key = event.target.closest('[data-character]');
  if (key) { event.preventDefault(); addCharacter(key.dataset.character); key.classList.remove('press-flash'); void key.offsetWidth; key.classList.add('press-flash'); }
});
document.addEventListener('click', event => {
  const key = event.target.closest('[data-character]');
  if (key && event.detail === 0) addCharacter(key.dataset.character);
});
$('#symbol-keyboard').addEventListener('click', () => { symbols = !symbols; renderKeyboard(); });
$('#keyboard-languages').addEventListener('click', () => openSheet('languages'));
$('#keyboard-layout').addEventListener('click', () => openSheet('layout'));
$('#keyboard-return').addEventListener('click', insertText);
function backspace() {
  const field = $('#compose');
  field.value = Array.from(field.value).slice(0, -1).join('');
  renderCandidates(); feedback('Backspace · custom key');
}
let eraseDelay; let eraseRepeat;
function stopErase() { clearTimeout(eraseDelay); clearInterval(eraseRepeat); }
$('#keyboard-backspace').addEventListener('pointerdown', event => { event.preventDefault(); event.currentTarget.setPointerCapture(event.pointerId); backspace(); stopErase(); eraseDelay = setTimeout(() => { eraseRepeat = setInterval(backspace, 70); }, 350); });
$('#keyboard-backspace').addEventListener('click', event => { if (event.detail === 0) backspace(); });
window.addEventListener('pointerup', stopErase); window.addEventListener('pointercancel', stopErase); window.addEventListener('blur', stopErase); document.addEventListener('visibilitychange', stopErase);
function renderCandidates() {
  const bar = $('#candidate-bar');
  const profile = currentProfile();
  const active = keyboardSource === 'custom' && ['zh', 'ja'].includes(profile.id);
  bar.hidden = !active; bar.replaceChildren();
  if (!active) return;
  const samples = profile.id === 'zh' ? { nihao: ['你好', '你', '好'], zhongwen: ['中文', '中国', '中'], ni: ['你', '呢', '尼'], hao: ['好', '号', '浩'] } : { nihon: ['日本', 'にほん', 'ニホン'], arigatou: ['ありがとう', '有難う'], konnichiwa: ['こんにちは', '今日は'] };
  const raw = $('#compose').value.trim().toLowerCase();
  const candidates = Object.hasOwn(samples, raw) ? samples[raw] : [];
  bar.append(el('span', 'DEMO', 'candidate-label'));
  if (!candidates.length) bar.append(el('span', raw ? 'No demo match · use native IME for full conversion' : profile.id === 'zh' ? 'nihao → 你好' : 'nihon → 日本', 'candidate-empty'));
  candidates.forEach(candidate => { const button = el('button', candidate); button.addEventListener('click', () => { $('#compose').value = candidate; renderCandidates(); feedback('Candidate selected · tap Insert'); }); bar.append(button); });
}
function setKeyboardSource(source) {
  if (composing) { feedback('Confirm the current IME composition first'); return; }
  release(); keyboardShift = false; keyboardSource = source;
  $('#custom-keyboard-panel').hidden = source !== 'custom';
  $('#compose').readOnly = source === 'custom';
  $('#compose').inputMode = source === 'custom' ? 'none' : 'text';
  $('#compose').placeholder = source === 'custom' ? 'Tap the keys below…' : 'Type with your device keyboard…';
  $('#custom-keyboard').classList.toggle('active', source === 'custom');
  $('#native-keyboard').classList.toggle('active', source === 'native');
  $('#custom-keyboard').setAttribute('aria-pressed', String(source === 'custom'));
  $('#native-keyboard').setAttribute('aria-pressed', String(source === 'native'));
  $('#device').classList.toggle('native-input', source === 'native');
  renderLanguages();
  if (source === 'native') $('#compose').focus(); else $('#compose').blur();
  feedback(source === 'custom' ? 'TopVNC keyboard active' : 'Native input requested · OS controls visibility');
}
function buildLayout(content) {
  content.append(el('p', `${currentProfile().name} · three rows, one space between keys. Edit characters to match your preferred layout.`, 'sheet-description'));
  const field = el('textarea', undefined, 'layout-editor'); field.value = profileRows().map(row => row.join(' ')).join('\n'); field.rows = 5; field.maxLength = 500; field.setAttribute('aria-label', 'Three keyboard rows, keys separated by spaces'); field.spellcheck = false;
  const status = el('p', 'Up to 14 keys per row. Control keys keep their fixed positions.', 'sheet-description'); status.setAttribute('role', 'status');
  const saveButton = el('button', 'Save keyboard layout', 'wide-button');
  saveButton.addEventListener('click', () => {
    const rows = field.value.trim().split(/\r?\n/).map(row => row.trim().split(/\s+/u));
    if (rows.length !== 3 || rows.some(row => row.length < 3 || row.length > 14 || row.some(key => key.length > 8))) { status.textContent = 'Use exactly three rows, 3–14 keys each, and up to 8 characters per key.'; return; }
    currentProfile().rows = rows; save(); renderKeyboard(); closeSheet(); feedback('Custom layout saved on this device');
  });
  content.append(field, status, saveButton, el('p', 'This configures key labels and demo text output. Character conversion and remote keyboard mapping still need implementation and device testing.', 'sheet-notice'));
}
const platformText = navigator.userAgentData?.platform ?? navigator.platform ?? '';
const mobileApple = /iPhone|iPad|iPod/.test(navigator.userAgent) || (/Mac/.test(platformText) && navigator.maxTouchPoints > 1);
const osName = mobileApple ? 'iOS' : /Android/.test(navigator.userAgent) ? 'Android' : /Win/.test(platformText) ? 'Windows' : /Mac/.test(platformText) ? 'macOS' : /Linux/.test(platformText) ? 'Linux' : 'device';
$('#native-keyboard').textContent = `Native · ${osName}`;
$('#native-keyboard').setAttribute('aria-label', `Switch to native ${osName} keyboard input`);
customization = createCustomization({
  el, $, orientation: () => orientation, close: closeSheet, feedback,
  switchOrientation: setOrientation,
  key: (value, button) => { if (composing) { feedback('Finish composing before using a custom key'); return; } pressKey(value, button); },
  text: value => { if (composing) { feedback('Finish composing before inserting custom text'); return; } $('#remote-output').textContent = value; release(); feedback('Custom text inserted in demo screen'); },
});
renderLanguages(); renderPins(); setMode('type'); setOrientation('portrait'); feedback('Try the TopVNC keyboard');
