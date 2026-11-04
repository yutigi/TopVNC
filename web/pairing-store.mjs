// Browser-held E2E pairing: the pinned host public key and this browser's
// private key. Script on this origin can read it; see the README trust limit.
const STORAGE_KEY = "topvnc.e2e.pairing.v1";
const KEY_PATTERN = /^[A-Za-z0-9_-]{43}$/;

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
export function loadPairing(storage) {
  try {
    const raw = storage?.getItem(STORAGE_KEY);
    if (typeof raw !== "string" || raw.length > 512) return undefined;
    const value = JSON.parse(raw);
    if (value?.version !== 1 || !isKey(value.host) || !isKey(value.browser)) return undefined;
    return { host: value.host, browser: value.browser };
  } catch {
    return undefined;
  }
}

export function savePairing(storage, pairing) {
  if (!storage || !isKey(pairing?.host) || !isKey(pairing?.browser)) {
    throw new Error("Invalid pairing");
  }
  storage.setItem(STORAGE_KEY, JSON.stringify({ version: 1, host: pairing.host, browser: pairing.browser }));
}

export function forgetPairing(storage) {
  try {
    storage?.removeItem(STORAGE_KEY);
  } catch {
    // Nothing persisted, or storage is blocked: either way nothing is paired.
  }
}
