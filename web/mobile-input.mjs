// UI input ownership. Protocol encoding and backpressure remain in the session.
export const MODIFIERS = { Ctrl: 0xffe3, Alt: 0xffe9, Shift: 0xffe1, Win: 0xffeb };
const KEYS = {
  Esc: 0xff1b, Escape: 0xff1b, Tab: 0xff09, Enter: 0xff0d, Space: 32,
  Backspace: 0xff08, Delete: 0xffff, Insert: 0xff63, CapsLock: 0xffe5,
  Home: 0xff50, End: 0xff57, PageUp: 0xff55, PageDown: 0xff56,
  ArrowLeft: 0xff51, ArrowUp: 0xff52, ArrowRight: 0xff53, ArrowDown: 0xff54,
  ...MODIFIERS,
};
export function keySym(key) {
  if (typeof key !== 'string') return undefined;
  if (Object.hasOwn(KEYS, key)) return KEYS[key];
  if (/^F(?:[1-9]|1[0-9]|2[0-4])$/.test(key)) return 0xffbd + Number(key.slice(1));
  const characters = [...key];
  if (characters.length !== 1) return undefined;
  const code = key.toLowerCase().codePointAt(0);
  return code <= 0xff ? code : 0x01000000 + code;
}

export function createMobileInput({ input, button, move, changed = () => {} }) {
  const modifiers = new Set();
  const owners = new Map();
  function emit(key, down) {
    if (key === 'Mouse1' || key === 'Mouse2') button(key === 'Mouse1' ? 1 : 4, down);
    else input({ kind: 'keysym', keysym: key, down });
  }
  function hold(id, key) {
    const binding = ['Mouse1', 'Mouse2'].includes(key) ? key : keySym(key);
    if (owners.has(id) || owners.size >= 32 || binding === undefined) return;
    const alreadyHeld = [...owners.values()].includes(binding);
    owners.set(id, binding);
    if (!alreadyHeld) emit(binding, true);
  }
  function up(id) {
    const key = owners.get(id);
    if (key === undefined) return;
    owners.delete(id);
    if (![...owners.values()].includes(key)) emit(key, false);
  }
  function clearModifiers() { if (modifiers.size) { modifiers.clear(); changed(); } }
  function tap(key) {
    if (typeof key !== 'string' || key.length > 64) return false;
    const parts = key === '+' ? [key] : key.split('+');
    const base = parts.pop();
    if (!base || keySym(base) === undefined || parts.some(part => !Object.hasOwn(MODIFIERS, part))) return false;
    const chord = [...new Set([...modifiers, ...parts]), base];
    chord.forEach((part, i) => hold(`tap-${i}`, part));
    chord.forEach((_, i) => up(`tap-${chord.length - 1 - i}`));
    clearModifiers();
    return true;
  }
  return {
    modifiers,
    toggle(name) {
      if (!Object.hasOwn(MODIFIERS, name)) return;
      if (modifiers.has(name)) modifiers.delete(name); else modifiers.add(name);
      changed();
    },
    clearModifiers, tap, hold, up,
    beginPhysical(code) {
      for (const name of modifiers) hold(`physical-${code}-${name}`, name);
      clearModifiers();
    },
    endPhysical(code) {
      for (const name of Object.keys(MODIFIERS).reverse()) up(`physical-${code}-${name}`);
    },
    text(value) {
      if (typeof value !== 'string' || !value || value.length > 2000) return false;
      // Text insertion must never inherit a modifier latch.
      clearModifiers();
      // Rust bounds each typeText call to 256 Unicode characters. Keep every
      // character in our bounded draft, including surrogate pairs across chunks.
      const characters = [...value];
      for (let index = 0; index < characters.length; index += 256) input({ kind: 'text', text: characters.slice(index, index + 256).join('') });
      return true;
    },
    move(dx, dy) { if (Number.isFinite(dx) && Number.isFinite(dy)) move(dx, dy); },
    release() { [...owners.keys()].reverse().forEach(up); clearModifiers(); },
  };
}
