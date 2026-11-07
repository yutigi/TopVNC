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
export function createHeldInputs() {
  const held = new Map();
  return {
    down(id, key) { if ((held.has(id) || held.size < 16) && normalizeFpsKey(key)) held.set(id, key); },
    up(id) { held.delete(id); },
    clear() { held.clear(); },
    keys() { return [...new Set(held.values())]; },
  };
}
export function createFpsController({ $, el, feedback }) {
  const panel = $('#fps-panel');
  const held = createHeldInputs();
  let enabled = false; let settings = defaultFpsSettings(); let look;
  let x = 50; let y = 50;
  function report() {
    const keys = held.keys();
    panel.querySelectorAll('[data-fps-action]').forEach(button => button.classList.toggle('is-held', keys.includes(settings.bindings[button.dataset.fpsAction])));
    const status = $('#fps-held');
    if (status) status.textContent = keys.length ? keys.join(' + ') : 'Touch & hold · demo only';
    if (keys.length) feedback(`Holding ${keys.join(' + ')} · demo`);
  }
  function release() {
    const hadKeys = held.keys().length > 0;
    held.clear(); look = undefined; report();
    if (hadKeys) feedback('FPS controls released · demo');
    $('#fps-look')?.classList.remove('is-looking');
  }
  function render() {
    panel.replaceChildren();
    const movement = el('div', undefined, 'fps-movement'); movement.setAttribute('aria-label', 'Movement controls');
    const directional = el('div', undefined, 'fps-directional');
    const utility = el('div', undefined, 'fps-utility');
    const actions = el('div', undefined, 'fps-actions'); actions.setAttribute('aria-label', 'Combat controls');
    function makeButton(id, title, key) {
      const button = el('button', undefined, `fps-key fps-${id}`); button.dataset.fpsAction = id;
      button.setAttribute('aria-label', `Hold ${title}: ${key}`);
      button.append(el('span', key === 'Mouse1' ? '◉' : key === 'Mouse2' ? '⊕' : key, 'fps-key-symbol'), el('small', title));
      return button;
    }
    FPS_ACTIONS.forEach(([id, title]) => {
      const button = makeButton(id, title, settings.bindings[id]);
      (['forward', 'left', 'back', 'right'].includes(id) ? directional : ['sprint', 'crouch'].includes(id) ? utility : actions).append(button);
    });
    movement.append(directional, utility);
    const lookPad = el('div', undefined, 'fps-look'); lookPad.id = 'fps-look'; lookPad.setAttribute('aria-label', 'Drag to look; use another finger for fire or movement');
    lookPad.append(el('span', '⊕'), el('strong', 'LOOK / AIM'), el('small', 'Drag here · fire with another finger'));
    const status = el('div', 'Touch & hold · demo only', 'fps-held'); status.id = 'fps-held'; status.setAttribute('role', 'status');
    panel.append(lookPad, movement, actions, status);
    lookPad.addEventListener('pointerdown', event => { if (!enabled) return; event.preventDefault(); lookPad.setPointerCapture(event.pointerId); look = { id: event.pointerId, x: event.clientX, y: event.clientY }; lookPad.classList.add('is-looking'); });
    lookPad.addEventListener('pointermove', event => {
      if (!enabled || look?.id !== event.pointerId) return;
      x = Math.max(15, Math.min(85, x + (event.clientX - look.x) * settings.sensitivity * 0.12));
      y = Math.max(20, Math.min(75, y + (event.clientY - look.y) * settings.sensitivity * 0.12));
      look.x = event.clientX; look.y = event.clientY;
      $('#fps-crosshair').style.left = `${x}%`; $('#fps-crosshair').style.top = `${y}%`;
    });
    function stopLook(event) { if (look?.id === event.pointerId) { look = undefined; lookPad.classList.remove('is-looking'); } }
    lookPad.addEventListener('pointerup', stopLook); lookPad.addEventListener('pointercancel', stopLook); lookPad.addEventListener('lostpointercapture', stopLook);
  }
  panel.addEventListener('pointerdown', event => {
    const button = event.target.closest('[data-fps-action]'); if (!enabled || !button) return;
    event.preventDefault(); button.setPointerCapture(event.pointerId); held.down(event.pointerId, settings.bindings[button.dataset.fpsAction]); report();
  });
  function pointerRelease(event) { held.up(event.pointerId); report(); }
  panel.addEventListener('pointerup', pointerRelease); panel.addEventListener('pointercancel', pointerRelease); panel.addEventListener('lostpointercapture', pointerRelease);
  panel.addEventListener('keydown', event => { const button = event.target.closest('[data-fps-action]'); if (!enabled || !button || ![' ', 'Enter'].includes(event.key)) return; event.preventDefault(); held.down(`keyboard-${button.dataset.fpsAction}`, settings.bindings[button.dataset.fpsAction]); report(); });
  panel.addEventListener('keyup', event => { const button = event.target.closest('[data-fps-action]'); if (!button || ![' ', 'Enter'].includes(event.key)) return; event.preventDefault(); held.up(`keyboard-${button.dataset.fpsAction}`); report(); });
  panel.addEventListener('focusout', event => { const button = event.target.closest('[data-fps-action]'); if (button) { held.up(`keyboard-${button.dataset.fpsAction}`); report(); } });
  window.addEventListener('blur', release); document.addEventListener('visibilitychange', release);
  return {
    release,
    apply(active, nextSettings) {
      release(); enabled = active; settings = normalizeFpsSettings(nextSettings);
      $('#device').classList.toggle('fps-active', active); $('#device').dataset.fpsHand = settings.handedness;
      panel.hidden = !active;
      if (active) render();
    },
  };
}
