export function createLanguageSwitcher(api) {
  const { key, host, el, profiles, current, choose, configure, canChange, beforeOpen, feedback } = api;
  const doc = api.document ?? document;
  const view = api.window ?? window;
  const popup = el('div', undefined, 'language-popover');
  popup.id = 'language-popover'; popup.hidden = true;
  popup.setAttribute('role', 'menu'); popup.setAttribute('aria-label', 'Keyboard languages');
  host.append(popup);
  key.setAttribute('aria-haspopup', 'menu'); key.setAttribute('aria-controls', popup.id); key.setAttribute('aria-expanded', 'false');
  let gesture; let timer; let items = []; let highlighted = -1;
  function ordered() { const all = profiles(); return [...all.filter(p => p.pinned), ...all.filter(p => !p.pinned)]; }
  function cancel(restoreFocus = false) {
    view.clearTimeout(timer); timer = undefined;
    const pointer = gesture?.id; gesture = undefined;
    if (pointer !== undefined && key.hasPointerCapture(pointer)) key.releasePointerCapture(pointer);
    popup.hidden = true; key.setAttribute('aria-expanded', 'false'); key.classList.remove('language-key-held');
    if (restoreFocus) key.focus();
  }
  function allowed() { if (canChange()) return true; feedback('Finish composing before changing language'); cancel(); return false; }
  function mark(index, focus = false) {
    highlighted = index;
    items.forEach((item, i) => { item.classList.toggle('language-hover', i === index); item.tabIndex = i === index ? 0 : -1; });
    if (focus && items[index]) { items[index].focus(); items[index].scrollIntoView({ block: 'nearest' }); }
  }
  function select(id) { if (!allowed()) return; cancel(true); choose(id); }
  function position() {
    const rect = host.getBoundingClientRect(); const anchor = key.getBoundingClientRect();
    const sx = host.offsetWidth / rect.width; const sy = host.offsetHeight / rect.height;
    const x = (anchor.left - rect.left) * sx - host.clientLeft;
    const y = (anchor.top - rect.top) * sy - host.clientTop;
    const bottom = (anchor.bottom - rect.top) * sy - host.clientTop;
    const width = Math.min(264, host.clientWidth - 24);
    popup.style.width = `${width}px`;
    popup.style.left = `${Math.max(12, Math.min(x, host.clientWidth - width - 12))}px`;
    const below = host.clientHeight - bottom - 12; const above = y - 12;
    const useBelow = above < 144 && below > above;
    popup.style.maxHeight = `${Math.max(0, useBelow ? below : above)}px`;
    popup.style.top = useBelow ? `${bottom + 6}px` : 'auto';
    popup.style.bottom = useBelow ? 'auto' : `${host.clientHeight - y + 6}px`;
  }
  function open(focus = false) {
    if (!allowed()) return;
    const title = el('div', 'Slide to a language', 'language-popover-title'); title.setAttribute('role', 'presentation');
    const list = el('div', undefined, 'language-popover-list'); list.setAttribute('role', 'group'); list.setAttribute('aria-label', 'Available languages');
    items = ordered().map(profile => {
      const item = el('button', undefined, 'language-menu-item'); item.type = 'button'; item.dataset.languageId = profile.id;
      item.setAttribute('role', 'menuitemradio'); item.setAttribute('aria-checked', String(profile.id === current()));
      const native = { en: 'EN', th: 'ไทย', zh: '中文', ja: '日本語' };
      const name = el('span', profile.name, 'language-menu-name');
      const sample = el('small', Object.hasOwn(native, profile.id) ? native[profile.id] : profile.label);
      name.append(sample); item.append(name, el('span', profile.id === current() ? '✓' : '', 'language-check'));
      item.addEventListener('click', () => select(profile.id)); list.append(item); return item;
    });
    const manage = el('button', 'Manage languages…', 'language-menu-manage'); manage.type = 'button'; manage.setAttribute('role', 'menuitem'); manage.dataset.languageManage = 'true';
    manage.addEventListener('click', () => { cancel(true); configure(); }); items.push(manage);
    popup.replaceChildren(title, list, manage); popup.hidden = false;
    key.setAttribute('aria-expanded', 'true'); key.classList.add('language-key-held');
    position(); mark(Math.max(0, ordered().findIndex(p => p.id === current())), focus);
  }
  function hit(event) {
    const node = doc.elementFromPoint(event.clientX, event.clientY);
    const item = node?.closest('[data-language-id], [data-language-manage]');
    return item && popup.contains(item) ? items.indexOf(item) : -1;
  }
  key.addEventListener('pointerdown', event => {
    if (event.button !== 0 || gesture || !allowed()) return;
    event.preventDefault(); beforeOpen();
    gesture = { id: event.pointerId, x: event.clientX, y: event.clientY, held: false, moved: false };
    key.setPointerCapture(event.pointerId);
    timer = view.setTimeout(() => { if (!gesture) return; gesture.held = true; open(); }, 350);
  });
  key.addEventListener('pointermove', event => {
    if (gesture?.id !== event.pointerId) return;
    if (Math.hypot(event.clientX - gesture.x, event.clientY - gesture.y) > 12) gesture.moved = true;
    if (gesture.held) mark(hit(event));
    else if (gesture.moved) { view.clearTimeout(timer); timer = undefined; }
  });
  key.addEventListener('pointerup', event => {
    if (gesture?.id !== event.pointerId) return;
    event.preventDefault(); view.clearTimeout(timer);
    const previous = gesture; const index = previous.held ? hit(event) : -1;
    gesture = undefined;
    if (key.hasPointerCapture(event.pointerId)) key.releasePointerCapture(event.pointerId);
    if (previous.held) {
      if (index >= 0) items[index].click();
      else if (!key.contains(doc.elementFromPoint(event.clientX, event.clientY))) cancel();
      // Lifting on the globe leaves the list open for tapping or scrolling.
    } else if (!previous.moved && allowed()) {
      const all = ordered(); const next = all[(all.findIndex(p => p.id === current()) + 1) % all.length];
      cancel(); if (next) choose(next.id);
    }
  });
  key.addEventListener('click', event => { if (event.detail === 0) { beforeOpen(); open(true); } });
  key.addEventListener('contextmenu', event => event.preventDefault());
  key.addEventListener('lostpointercapture', () => { if (gesture) cancel(); });
  key.addEventListener('keydown', event => { if (['ArrowDown', 'ArrowUp'].includes(event.key)) { event.preventDefault(); beforeOpen(); open(true); } });
  popup.addEventListener('pointermove', event => { if (!gesture) mark(hit(event)); });
  popup.addEventListener('keydown', event => {
    if (['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) {
      event.preventDefault();
      mark(event.key === 'Home' ? 0 : event.key === 'End' ? items.length - 1 : (highlighted + (event.key === 'ArrowDown' ? 1 : -1) + items.length) % items.length, true);
    } else if (event.key === 'Escape') { event.preventDefault(); event.stopPropagation(); cancel(true); }
    else if (event.key === 'Tab') cancel(true);
  });
  doc.addEventListener('pointerdown', event => {
    if (!popup.hidden && !popup.contains(event.target) && !key.contains(event.target)) { cancel(); event.preventDefault(); event.stopPropagation(); }
  }, true);
  doc.addEventListener('focusin', event => { if (!popup.hidden && !popup.contains(event.target) && !key.contains(event.target)) cancel(); });
  for (const name of ['blur', 'pagehide', 'pointercancel', 'resize']) view.addEventListener(name, () => cancel());
  doc.addEventListener('visibilitychange', () => cancel());
  host.addEventListener('scroll', event => { if (!popup.contains(event.target)) cancel(); }, true);
  return { cancel };
}
