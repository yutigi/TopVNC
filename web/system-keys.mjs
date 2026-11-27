// Browsers leave shortcuts with the Windows key (Command on a Mac), such as
// Win+Space to switch the input language, to the operating system, so the
// page never sees them. Keyboard Lock, in Chromium-based browsers, takes those
// keys for the page, but only while it is in fullscreen. Win+L and
// Ctrl+Alt+Del always stay with the operating system.

export const SYSTEM_KEYS = Object.freeze(["MetaLeft", "MetaRight"]);

// Lock the system keys while the viewer is in fullscreen and the setting is
// on, otherwise release them. Returns whether a lock was requested.
export function lockSystemKeys(keyboard, { fullscreen, enabled }) {
  if (typeof keyboard?.lock !== "function" || typeof keyboard.unlock !== "function") return false;
  if (!fullscreen || !enabled) {
    keyboard.unlock();
    return false;
  }
  try {
    // Rejected, for example, in a frame or after fullscreen ended; the keys
    // then simply stay with the operating system.
    Promise.resolve(keyboard.lock([...SYSTEM_KEYS])).catch(() => {});
  } catch {
    return false;
  }
  return true;
}
