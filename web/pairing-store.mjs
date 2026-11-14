// Browser-held E2E pairing: the pinned host public key and this browser's
// private key. Script on this origin can read it; see the README trust limit.
const STORAGE_KEY = "topvnc.e2e.pairing.v1";
const KEY_PATTERN = /^[A-Za-z0-9_-]{43}$/;
export const HOST_ID_PATTERN = /^[a-z0-9][a-z0-9-]{0,47}$/;

function storageKey(hostId) {
  if (hostId === undefined) return STORAGE_KEY;
  if (typeof hostId !== "string" || !HOST_ID_PATTERN.test(hostId)) throw new Error("Invalid machine ID");
  return `topvnc.e2e.pairing.v2.${hostId}`;
}

export function isKey(value) {
  return typeof value === "string" && KEY_PATTERN.test(value);
}

// The invite text from a `#pair=` fragment. Fragments are not sent in requests.
export function inviteFromHash(hash) {
  if (typeof hash !== "string" || !hash.startsWith("#pair=")) return undefined;
  const text = hash.slice("#pair=".length);
  return text.length > 0 && text.length <= 128 ? text : undefined;
}

// Missing, unreadable, or altered state means "not paired": fail closed.
export function loadPairing(storage, hostId) {
  try {
    const raw = storage?.getItem(storageKey(hostId));
    if (typeof raw !== "string" || raw.length > 512) return undefined;
    const value = JSON.parse(raw);
    if (value?.version !== 1 || !isKey(value.host) || !isKey(value.browser)) return undefined;
    return { host: value.host, browser: value.browser };
  } catch {
    return undefined;
  }
}

export function savePairing(storage, pairing, hostId) {
  if (!storage || !isKey(pairing?.host) || !isKey(pairing?.browser)) {
    throw new Error("Invalid pairing");
  }
  storage.setItem(storageKey(hostId), JSON.stringify({ version: 1, host: pairing.host, browser: pairing.browser }));
}

export function forgetPairing(storage, hostId) {
  try {
    storage?.removeItem(storageKey(hostId));
  } catch {
    // Nothing persisted, or storage is blocked: either way nothing is paired.
  }
}

// The deployment owner must name the old machine explicitly. Never guess from
// ordering or the currently selected host. Keep existing scoped pins intact.
export function migratePairing(storage, hostId) {
  if (typeof hostId !== "string" || !HOST_ID_PATTERN.test(hostId)) return;
  const legacy = loadPairing(storage);
  if (!legacy) return;
  const current = loadPairing(storage, hostId);
  if (current && (current.host !== legacy.host || current.browser !== legacy.browser)) return;
  savePairing(storage, legacy, hostId); // A failed write must leave the old key intact.
  forgetPairing(storage);
}
