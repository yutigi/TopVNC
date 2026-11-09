export const FPS_ACTIONS = [
  ['forward', 'Forward', 'W'], ['left', 'Left', 'A'], ['back', 'Back', 'S'], ['right', 'Right', 'D'],
  ['sprint', 'Sprint', 'Shift'], ['crouch', 'Crouch', 'Ctrl'], ['fire', 'Fire', 'Mouse1'],
  ['aim', 'Aim', 'Mouse2'], ['jump', 'Jump', 'Space'], ['reload', 'Reload', 'R'],
  ['interact', 'Use', 'E'], ['weapon', 'Weapon', '1'],
];
export function defaultFpsSettings() {
  return { handedness: 'right', sensitivity: 1, bindings: Object.fromEntries(FPS_ACTIONS.map(([id, , key]) => [id, key])) };
}
export function normalizeFpsKey(value) {
  if (typeof value !== 'string' || value.length > 16) return '';
  const keys = ['Shift', 'Ctrl', 'Alt', 'Win', 'Space', 'Tab', 'Esc', 'Enter', 'Backspace', 'Mouse1', 'Mouse2', 'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight'];
  return keys.find(key => key.toLowerCase() === value.trim().toLowerCase()) ?? (/^(?:[a-z0-9]|f(?:[1-9]|1[0-9]|2[0-4]))$/i.test(value.trim()) ? value.trim().toUpperCase() : '');
}
export function normalizeFpsSettings(value) {
  const result = defaultFpsSettings();
  if (!value || typeof value !== 'object') return result;
  if (['left', 'right'].includes(value.handedness)) result.handedness = value.handedness;
  if (Number.isFinite(value.sensitivity) && value.sensitivity >= 0.5 && value.sensitivity <= 2) result.sensitivity = value.sensitivity;
  for (const [id] of FPS_ACTIONS) {
    const key = normalizeFpsKey(value.bindings?.[id]);
    if (key) result.bindings[id] = key;
  }
  return result;
}
// Each pointer owns its hold. Shared bindings are reference-counted by input.
export function createFpsController({ $, el, input, active }) {
  const panel = $('#fps-panel');
  let enabled = false; let settings = defaultFpsSettings(); let look;
  const held = new Map();
  function releasePointer(id) {
    input.up(`fps-${id}`); held.delete(id);
    panel.querySelectorAll('[data-fps-action]').forEach(button => button.classList.toggle('is-held', [...held.values()].includes(button.dataset.fpsAction)));
  }
  function release() {
    [...held.keys()].forEach(releasePointer); look = undefined;
    $('#fps-look')?.classList.remove('is-looking');
  }
  function render() {
    panel.replaceChildren();
    const movement = el('div', undefined, 'fps-movement');
    const directional = el('div', undefined, 'fps-directional');
    const utility = el('div', undefined, 'fps-utility');
    const actions = el('div', undefined, 'fps-actions');
    FPS_ACTIONS.forEach(([id, title]) => {
      const button = el('button', undefined, `fps-key fps-${id}`);
      button.dataset.fpsAction = id;
      button.setAttribute('aria-label', `Hold ${title}: ${settings.bindings[id]}`);
      button.append(el('span', settings.bindings[id]), el('small', title));
      (['forward', 'left', 'back', 'right'].includes(id) ? directional : ['sprint', 'crouch'].includes(id) ? utility : actions).append(button);
    });
    movement.append(directional, utility);
    const pad = el('div', undefined, 'fps-look'); pad.id = 'fps-look';
    pad.setAttribute('aria-label', 'Drag to look; use another finger to fire or move');
    pad.append(el('strong', 'LOOK / AIM'), el('small', 'Drag here'));
    panel.append(movement, pad, actions);
    pad.addEventListener('pointerdown', event => {
      if (!enabled || !active() || look) return;
      event.preventDefault(); pad.setPointerCapture(event.pointerId);
      look = { id: event.pointerId, x: event.clientX, y: event.clientY }; pad.classList.add('is-looking');
    });
    pad.addEventListener('pointermove', event => {
      if (!enabled || !active() || look?.id !== event.pointerId) return;
      input.move((event.clientX - look.x) * settings.sensitivity, (event.clientY - look.y) * settings.sensitivity);
      look.x = event.clientX; look.y = event.clientY;
    });
    for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) pad.addEventListener(name, event => {
      if (look?.id === event.pointerId) { look = undefined; pad.classList.remove('is-looking'); }
    });
  }
  function down(id, button) {
    if (!enabled || !active() || held.has(id) || held.size >= 16) return;
    held.set(id, button.dataset.fpsAction);
    input.hold(`fps-${id}`, settings.bindings[button.dataset.fpsAction]); button.classList.add('is-held');
  }
  panel.addEventListener('pointerdown', event => {
    const button = event.target.closest('[data-fps-action]'); if (!button || !enabled || !active()) return;
    event.preventDefault(); button.setPointerCapture(event.pointerId); down(event.pointerId, button);
  });
  for (const name of ['pointerup', 'pointercancel', 'lostpointercapture']) panel.addEventListener(name, event => releasePointer(event.pointerId));
  panel.addEventListener('keydown', event => {
    const button = event.target.closest('[data-fps-action]'); if (!button || ![' ', 'Enter'].includes(event.key)) return;
    event.preventDefault(); down(`keyboard-${button.dataset.fpsAction}`, button);
  });
  panel.addEventListener('keyup', event => {
    const button = event.target.closest('[data-fps-action]'); if (!button || ![' ', 'Enter'].includes(event.key)) return;
    event.preventDefault(); releasePointer(`keyboard-${button.dataset.fpsAction}`);
  });
  panel.addEventListener('focusout', event => {
    const button = event.target.closest('[data-fps-action]'); if (button) releasePointer(`keyboard-${button.dataset.fpsAction}`);
  });
  return {
    release,
    apply(next, config) {
      // Applying layout does not interrupt a hold unless configuration changes.
      const normalized = normalizeFpsSettings(config);
      if (enabled === next && JSON.stringify(normalized) === JSON.stringify(settings) && panel.children.length) return;
      release(); enabled = next; settings = normalized;
      $('#device').classList.toggle('fps-active', next); $('#device').dataset.fpsHand = settings.handedness;
      panel.hidden = !next; if (next) render();
    },
  };
}
