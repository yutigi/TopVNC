import { defaultFpsSettings, normalizeFpsSettings, normalizeFpsKey, FPS_ACTIONS } from './fps.mjs';
const STORAGE_KEY = 'topvnc.mobile.customize.v1';
const GROUPS = ['modifiers', 'actions', 'input'];
const DOCK = ['keys', 'type', 'pointer', 'enter'];
const GROUP_NAMES = { modifiers: 'Type · Ctrl / Alt / Shift', actions: 'Your custom keys', input: 'Keyboard / active input' };
const DOCK_NAMES = { keys: 'Keys', type: 'Type', pointer: 'Pointer', enter: 'Enter' };
export function defaultCustomization() {
  const layout = () => ({ position: 'auto', groups: [...GROUPS], dock: [...DOCK], showActions: true, size: 'standard' });
  return {
    actions: [],
    preset: 'standard', fps: defaultFpsSettings(),
    layouts: { portrait: layout(), landscape: layout() },
  };
}
export function normalizeChord(value) {
  if (typeof value !== 'string' || value.length > 64) return '';
  const aliases = { ctrl: 'Ctrl', control: 'Ctrl', alt: 'Alt', shift: 'Shift', win: 'Win', meta: 'Win', cmd: 'Win', command: 'Win', esc: 'Esc', escape: 'Esc', del: 'Delete', delete: 'Delete', space: 'Space', spacebar: 'Space', enter: 'Enter', return: 'Enter', tab: 'Tab', backspace: 'Backspace', home: 'Home', end: 'End', pageup: 'PageUp', pagedown: 'PageDown', insert: 'Insert', capslock: 'CapsLock', arrowup: 'ArrowUp', arrowdown: 'ArrowDown', arrowleft: 'ArrowLeft', arrowright: 'ArrowRight' };
  const parts = value.split('+').map(part => part.trim());
  if (parts.length > 5 || parts.some(part => !part)) return '';
  const normalized = parts.map(part => Object.hasOwn(aliases, part.toLowerCase()) ? aliases[part.toLowerCase()] : (/^(?:[a-z0-9]|f(?:[1-9]|1[0-9]|2[0-4]))$/i.test(part) ? part.toUpperCase() : ''));
  if (normalized.some(part => !part) || new Set(normalized).size !== normalized.length) return '';
  const modifiers = ['Ctrl', 'Alt', 'Shift', 'Win'];
  if (normalized.slice(0, -1).some(part => !modifiers.includes(part)) || modifiers.includes(normalized.at(-1))) return '';
  return normalized.join('+');
}
function isAction(action) {
  return action && typeof action.id === 'string' && action.id.length > 0 && action.id.length <= 64 && typeof action.label === 'string' && action.label.trim().length > 0 && action.label.length <= 24 && typeof action.wide === 'boolean' && typeof action.value === 'string' && (action.type === 'key' ? !!normalizeChord(action.value) : action.type === 'text' && action.value.length > 0 && action.value.length <= 200);
}
function permutation(value, expected) {
  return Array.isArray(value) && value.length === expected.length && new Set(value).size === expected.length && value.every(item => expected.includes(item));
}
export function normalizeCustomization(value) {
  const result = defaultCustomization();
  if (!value || typeof value !== 'object') return result;
  result.preset = value.preset === 'fps' ? 'fps' : 'standard';
  result.fps = normalizeFpsSettings(value.fps);
  if (Array.isArray(value.actions) && value.actions.length <= 8 && value.actions.every(isAction) && new Set(value.actions.map(action => action.id)).size === value.actions.length) {
    result.actions = value.actions.map(({ id, label, type, value: actionValue, wide }) => ({ id, label, type, value: type === 'key' ? normalizeChord(actionValue) : actionValue, wide }));
  }
  for (const orientation of ['portrait', 'landscape']) {
    const input = value.layouts?.[orientation]; const layout = result.layouts[orientation];
    if (!input || typeof input !== 'object') continue;
    // Preserve the old row's position, but never apply its hide-language preference to modifiers.
    const groups = Array.isArray(input.groups) ? input.groups.map(group => group === 'languages' ? 'modifiers' : group) : [];
    if (permutation(groups, GROUPS)) layout.groups = [...groups];
    if (permutation(input.dock, DOCK)) layout.dock = [...input.dock];
    if ((orientation === 'portrait' ? ['auto', 'top', 'bottom'] : ['auto', 'left', 'right', 'bottom']).includes(input.position)) layout.position = input.position;
    if (['standard', 'large'].includes(input.size)) layout.size = input.size;
    if (typeof input.showActions === 'boolean') layout.showActions = input.showActions;
  }
  return result;
}
export function moveItem(items, index, direction) {
  const next = [...items]; const target = index + direction;
  if (!Number.isInteger(index) || ![-1, 1].includes(direction) || index < 0 || index >= next.length || target < 0 || target >= next.length) return next;
  [next[index], next[target]] = [next[target], next[index]];
  return next;
}

export function createCustomization(api) {
  const { el, $ } = api;
  let state = defaultCustomization();
  try {
    const saved = api.storage?.getItem(STORAGE_KEY) ?? api.storage?.getItem('topvnc-wireframe-customize-v1');
    if (saved && saved.length <= 65536) state = normalizeCustomization(JSON.parse(saved));
  } catch { /* Preferences remain usable without storage. */ }
  let storageAvailable = true;
  function persist() {
    try { if (!api.storage) throw new Error('Storage unavailable'); api.storage.setItem(STORAGE_KEY, JSON.stringify(state)); storageAvailable = true; } catch { storageAvailable = false; }
  }
  function apply(orientation = api.orientation()) {
    const layout = state.layouts[orientation]; const device = $('#device');
    $('#landscape-controls').value = state.layouts.landscape.position;
    device.dataset.layoutPosition = layout.position;
    device.dataset.keySize = layout.size;
    const groups = { modifiers: [$('.type-modifiers')], actions: [$('#custom-action-pad')], input: [$('#keys-panel'), $('#type-panel'), $('#pointer-panel')] };
    layout.groups.forEach((group, index) => groups[group].forEach(node => { node.style.order = String(index * 10); }));
    const typing = api.mode() === 'type';
    $('.type-modifiers').hidden = !typing;
    const pad = $('#custom-action-pad'); pad.replaceChildren(); pad.hidden = !layout.showActions || !state.actions.length;
    state.actions.forEach(action => {
      const button = el('button', action.label, action.wide ? 'custom-key wide-key' : 'custom-key');
      button.title = action.type === 'key' ? action.value : 'Insert saved text';
      button.setAttribute('aria-label', `${action.label}: ${action.type === 'key' ? action.value : 'insert text snippet'}`);
      button.addEventListener('click', () => action.type === 'key' ? api.key(action.value, button) : api.text(action.value));
      pad.append(button);
    });
    const dockNodes = { keys: $('[data-mode="keys"]'), type: $('[data-mode="type"]'), pointer: $('[data-mode="pointer"]'), enter: $('.dock-enter') };
    layout.dock.forEach(name => $('.thumb-dock').append(dockNodes[name]));
    const fpsActive = state.preset === 'fps' && api.mode() === 'pointer';
    dockNodes.pointer.querySelector('small').textContent = state.preset === 'fps' ? 'Play' : 'Pointer';
    dockNodes.pointer.querySelector('span').textContent = state.preset === 'fps' ? '⊕' : '↖';
    api.fps(fpsActive, state.fps);
    $('#pointer-panel').hidden = api.mode() !== 'pointer' || fpsActive;
  }
  function usePreset(preset) {
    if (!api.canChange()) { api.feedback('Finish composing before changing preset'); return; }
    state.preset = preset === 'fps' ? 'fps' : 'standard'; persist(); api.close(); api.enterPreset(state.preset); apply();
  }
  function setLandscapePosition(position) {
    if (!api.canChange()) {
      api.feedback('Finish composing before changing layout.'); apply(); return;
    }
    if (!['auto', 'left', 'right', 'bottom'].includes(position)) { apply(); return; }
    state.layouts.landscape.position = position;
    persist(); apply();
    if (!storageAvailable) api.feedback('Storage unavailable: layout changes last for this session.');
  }
  function build(content) {
    let tab = 'keys'; let editingId;
    let draftLayouts = structuredClone(state.layouts);
    let editingOrientation = api.orientation();
    content.replaceChildren();
    const tabs = el('div', undefined, 'customize-tabs'); tabs.setAttribute('aria-label', 'Customization sections');
    const body = el('div', undefined, 'customize-body');
    const status = el('p', '', 'customize-status'); status.setAttribute('role', 'status');
    const presetTab = el('button', 'Presets'); const keyTab = el('button', 'Your keys'); const layoutTab = el('button', 'UI layout');
    tabs.append(presetTab, keyTab, layoutTab); content.append(tabs, status, body);
    function message(text) { status.textContent = text + (storageAvailable ? '' : ' Storage unavailable: changes last for this session.'); }
    function show(next) {
      tab = next; presetTab.setAttribute('aria-pressed', String(tab === 'presets')); keyTab.setAttribute('aria-pressed', String(tab === 'keys')); layoutTab.setAttribute('aria-pressed', String(tab === 'layout'));
      body.replaceChildren();
      if (tab === 'presets') buildPresets(); else if (tab === 'keys') buildKeys(); else buildLayoutMenu();
    }
    presetTab.addEventListener('click', () => show('presets')); keyTab.addEventListener('click', () => show('keys')); layoutTab.addEventListener('click', () => show('layout'));
    function buildPresets() {
      const draft = structuredClone(state.fps);
      const card = el('div', undefined, 'fps-preset-card');
      card.append(el('span', '01 / GAME PRESET', 'section-label'), el('h4', 'FPS · two-thumb controls'), el('p', 'Hold WASD to move. Drag the look zone while another finger fires. Jump, reload, sprint and crouch stay within reach. No language pins in play.', 'sheet-description'));
      const use = el('button', state.preset === 'fps' ? 'Open FPS preset →' : 'Use FPS preset →', 'wide-button');
      const fields = new Map();
      function readDraft() {
        for (const [id, input] of fields) { const value = normalizeFpsKey(input.value); if (!value) { message(`Choose a valid key for ${id}. Examples: W, Shift, Space, Mouse1.`); input.focus(); return false; } draft.bindings[id] = value; }
        state.fps = normalizeFpsSettings(draft); return true;
      }
      use.addEventListener('click', () => { if (readDraft()) usePreset('fps'); }); card.append(use); body.append(card);
      const handLabel = el('label', 'Aiming hand', 'customize-field'); const hand = el('select');
      [['right', 'Right thumb aims · WASD on left'], ['left', 'Left thumb aims · WASD on right']].forEach(([value, title]) => { const option = el('option', title); option.value = value; hand.append(option); }); hand.value = draft.handedness; hand.addEventListener('change', () => { draft.handedness = hand.value; }); handLabel.append(hand); body.append(handLabel);
      const sensitivityLabel = el('label', 'Look sensitivity', 'customize-field'); const sensitivity = el('input'); sensitivity.type = 'range'; sensitivity.min = '0.5'; sensitivity.max = '2'; sensitivity.step = '0.1'; sensitivity.value = String(draft.sensitivity); const output = el('output', `${draft.sensitivity.toFixed(1)}×`); sensitivity.addEventListener('input', () => { draft.sensitivity = Number(sensitivity.value); output.textContent = `${draft.sensitivity.toFixed(1)}×`; }); sensitivityLabel.append(sensitivity, output); body.append(sensitivityLabel, el('h4', 'Remap game controls', 'customize-subheading'));
      const mapping = el('div', undefined, 'fps-binding-editor');
      FPS_ACTIONS.forEach(([id, title]) => { const label = el('label', title); const input = el('input'); input.value = draft.bindings[id]; input.maxLength = 16; input.setAttribute('aria-label', `${title} binding`); input.autocomplete = 'off'; label.append(input); fields.set(id, input); mapping.append(label); }); body.append(mapping);
      const save = el('button', 'Save FPS controls', 'wide-button'); save.addEventListener('click', () => { if (readDraft()) { persist(); apply(); message('FPS controls saved on this device.'); } });
      const standard = el('button', 'Use standard controls', 'cancel-customize'); standard.addEventListener('click', () => usePreset('standard'));
      body.append(save, standard, el('p', 'These controls send input to your connected host. Games differ in relative pointer support. Your custom keys, typing languages and standard UI layouts are preserved.', 'sheet-notice'));
    }
    function reorderList(items, names, onMove, onEdit, onRemove) {
      const list = el('div', undefined, 'reorder-list');
      items.forEach((id, index) => {
        const row = el('div', undefined, 'reorder-row');
        const label = el('span', names[id], 'reorder-name');
        const up = el('button', '↑'); const down = el('button', '↓');
        up.disabled = index === 0; down.disabled = index === items.length - 1;
        up.setAttribute('aria-label', `Move ${names[id]} up`); down.setAttribute('aria-label', `Move ${names[id]} down`);
        up.addEventListener('click', () => onMove(index, -1)); down.addEventListener('click', () => onMove(index, 1));
        row.append(label, up, down);
        if (onEdit) { const edit = el('button', 'Edit'); edit.setAttribute('aria-label', `Edit ${names[id]}`); edit.addEventListener('click', () => onEdit(id)); row.append(edit); }
        if (onRemove) { const remove = el('button', '×'); remove.setAttribute('aria-label', `Remove ${names[id]}`); remove.addEventListener('click', () => onRemove(id)); row.append(remove); }
        list.append(row);
      });
      return list;
    }
    function buildKeys() {
      body.append(el('p', 'Make a key for any shortcut or text snippet. Move keys into the order your thumbs remember. Up to eight keys.', 'sheet-description'));
      const names = Object.fromEntries(state.actions.map(action => [action.id, action.label]));
      body.append(reorderList(state.actions.map(action => action.id), names,
        (index, direction) => { state.actions = moveItem(state.actions, index, direction); persist(); apply(); show('keys'); message('Key order saved.'); },
        id => { editingId = id; show('keys'); body.querySelector('input').focus(); },
        id => { state.actions = state.actions.filter(action => action.id !== id); if (editingId === id) editingId = undefined; persist(); apply(); show('keys'); message('Key removed. Add it again at any time.'); }));
      const editing = state.actions.find(action => action.id === editingId);
      body.append(el('h4', editing ? `Edit “${editing.label}”` : 'Create a key', 'customize-subheading'));
      const form = el('form', undefined, 'custom-key-form');
      function labeled(title, field) { const label = el('label', title); label.append(field); form.append(label); return field; }
      const name = labeled('Button label', el('input')); name.maxLength = 24; name.required = true; name.placeholder = 'e.g. Push to talk'; name.value = editing?.label ?? '';
      const type = labeled('Action', el('select'));
      [['key', 'Key / shortcut'], ['text', 'Insert text snippet']].forEach(([value, label]) => { const option = el('option', label); option.value = value; type.append(option); }); type.value = editing?.type ?? 'key';
      const value = labeled('Key or shortcut', el('input')); value.maxLength = 64; value.placeholder = 'e.g. V, F5, Ctrl+Shift+S'; value.value = editing?.type === 'key' ? editing.value : '';
      const text = labeled('Text to send to the host', el('textarea')); text.maxLength = 200; text.rows = 2; text.placeholder = 'Your phrase…'; text.value = editing?.type === 'text' ? editing.value : '';
      const wideLabel = el('label', undefined, 'customize-check'); const wide = el('input'); wide.type = 'checkbox'; wide.checked = editing?.wide ?? false; wideLabel.append(wide, el('span', 'Wide button (two columns)')); form.append(wideLabel);
      function toggleAction() { value.parentElement.hidden = type.value !== 'key'; text.parentElement.hidden = type.value !== 'text'; value.required = type.value === 'key'; text.required = type.value === 'text'; }
      type.addEventListener('change', toggleAction); toggleAction();
      const submit = el('button', editing ? 'Save key' : 'Add key', 'wide-button'); submit.type = 'submit'; form.append(submit);
      if (editing) { const cancel = el('button', 'Cancel edit', 'cancel-customize'); cancel.type = 'button'; cancel.addEventListener('click', () => { editingId = undefined; show('keys'); }); form.append(cancel); }
      form.addEventListener('submit', event => {
        event.preventDefault();
        if (!editing && state.actions.length >= 8) { message('Eight keys are already configured. Remove one to make room.'); return; }
        const actionValue = type.value === 'key' ? normalizeChord(value.value) : text.value;
        if (!name.value.trim() || !actionValue || actionValue.length > (type.value === 'key' ? 64 : 200)) { message('Use a label and a valid key, shortcut, or text snippet. Example: Ctrl+Shift+S.'); return; }
        const action = { id: editing?.id ?? `key-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`, label: name.value.trim(), type: type.value, value: actionValue, wide: wide.checked };
        if (editing) state.actions[state.actions.findIndex(item => item.id === editing.id)] = action; else state.actions.push(action);
        editingId = undefined; persist(); apply(); show('keys'); message(`${action.label} saved. Open the viewer to try it.`);
      });
      body.append(form, el('p', 'Keys and snippets send input to the connected host. Snippets are stored only in this browser.', 'sheet-notice'));
      const done = el('button', 'Done · try my keys', 'wide-button'); done.addEventListener('click', api.close); body.append(done);
    }
    function buildLayoutMenu() {
      const layout = draftLayouts[editingOrientation];
      body.append(el('p', 'Arrange standard controls per orientation. Ctrl, Alt and Shift appear only in Type; switch languages from the keyboard icon. FPS Play uses its own thumb controls; customize that in Presets.', 'sheet-description'));
      const orientationButtons = el('div', undefined, 'customize-tabs');
      ['portrait', 'landscape'].forEach(value => { const button = el('button', value === 'portrait' ? '▯ Portrait' : '▭ Landscape'); button.setAttribute('aria-pressed', String(editingOrientation === value)); button.addEventListener('click', () => { editingOrientation = value; show('layout'); }); orientationButtons.append(button); }); body.append(orientationButtons);
      const field = el('label', 'Control deck position', 'customize-field'); const position = el('select');
      (editingOrientation === 'portrait' ? [['auto', 'Default · bottom'], ['bottom', 'Bottom'], ['top', 'Top']] : [['auto', 'Default · adapt to input'], ['left', 'Left side'], ['right', 'Right side'], ['bottom', 'Bottom']]).forEach(([value, name]) => { const option = el('option', name); option.value = value; position.append(option); }); position.value = layout.position;
      position.addEventListener('change', () => { layout.position = position.value; updateSketch(); }); field.append(position); body.append(field);
      const sketch = el('div', undefined, 'layout-sketch'); sketch.setAttribute('aria-label', 'Layout order preview');
      function updateSketch() { sketch.replaceChildren(el('span', `REMOTE VIEW · ${layout.position.toUpperCase()} DECK`, 'sketch-screen')); layout.groups.forEach(group => { if (group === 'actions' && !layout.showActions) return; sketch.append(el('span', GROUP_NAMES[group], 'sketch-group')); }); sketch.append(el('span', layout.dock.map(id => DOCK_NAMES[id]).join('  ·  '), 'sketch-dock')); }
      updateSketch(); body.append(sketch, el('h4', 'Control group order', 'customize-subheading'));
      body.append(reorderList(layout.groups, GROUP_NAMES, (index, direction) => { layout.groups = moveItem(layout.groups, index, direction); show('layout'); }));
      body.append(el('h4', 'Thumb dock order', 'customize-subheading'));
      body.append(reorderList(layout.dock, DOCK_NAMES, (index, direction) => { layout.dock = moveItem(layout.dock, index, direction); show('layout'); }));
      for (const [key, title] of [['showActions', 'Show custom key pad']]) { const label = el('label', undefined, 'customize-check'); const checkbox = el('input'); checkbox.type = 'checkbox'; checkbox.checked = layout[key]; checkbox.addEventListener('change', () => { layout[key] = checkbox.checked; updateSketch(); }); label.append(checkbox, el('span', title)); body.append(label); }
      const sizeLabel = el('label', 'Custom key size', 'customize-field'); const size = el('select'); [['standard', 'Standard · 44 px'], ['large', 'Large · 52 px']].forEach(([value, title]) => { const option = el('option', title); option.value = value; size.append(option); }); size.value = layout.size; size.addEventListener('change', () => { layout.size = size.value; }); sizeLabel.append(size); body.append(sizeLabel);
      const save = el('button', 'Save layout · view changes', 'wide-button'); save.addEventListener('click', () => { state.layouts = structuredClone(draftLayouts); persist(); api.close(); apply(); api.feedback('Your layout saved · stable until you change it'); });
      const reset = el('button', 'Reset this orientation to defaults', 'cancel-customize'); reset.addEventListener('click', () => { draftLayouts[editingOrientation] = defaultCustomization().layouts[editingOrientation]; show('layout'); message('Default layout selected. Press Save layout to apply.'); });
      body.append(save, reset, el('p', 'Keys and the layout for the other orientation are kept. Customize and all four mode buttons always remain available.', 'sheet-notice'));
    }
    show('presets');
  }
  return { apply, build, usePreset, setLandscapePosition, isFps: () => state.preset === 'fps' };
}
