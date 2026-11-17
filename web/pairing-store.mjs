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

const samePairing = (a, b) => a.host === b.host && a.browser === b.browser;

// Forgetting a machine also forgets the v1 pairing whenever the next load
// would otherwise bring it back: when this machine is the migration target
// (even if it holds a different pairing), or when v1 is a copy of its pairing.
export function forgetPairing(storage, hostId, legacyHostId) {
  try {
    const scoped = hostId === undefined ? undefined : loadPairing(storage, hostId);
    const legacy = scoped && loadPairing(storage);
    // Remove v1 first: if that fails, the machine still shows as paired, not half-forgotten.
    if (legacy && (hostId === legacyHostId || samePairing(scoped, legacy))) storage.removeItem(STORAGE_KEY);
    storage?.removeItem(storageKey(hostId));
  } catch {
    // Nothing persisted, or storage is blocked: either way nothing is paired.
  }
}

// The deployment owner must name the old machine explicitly. Never guess from
// ordering or the currently selected host. Keep existing scoped pins intact.
// The v1 entry is copied, not moved: a rollback to the single-host website
// reads only v1, so removing it would unpair every migrated browser.
export function migratePairing(storage, hostId) {
  if (typeof hostId !== "string" || !HOST_ID_PATTERN.test(hostId)) return;
  const legacy = loadPairing(storage);
  if (!legacy || loadPairing(storage, hostId)) return;
  savePairing(storage, legacy, hostId);
}

// In registry mode, a v1 pairing that no machine uses: there is no migration
// target, or that machine already holds a different pairing. The page offers
// to remove it on its own, keeping the target's pairing. An empty target slot
// is not stranded: the next load migrates into it.
export function strandedLegacyPairing(storage, legacyHostId) {
  const legacy = loadPairing(storage);
  if (!legacy) return false;
  if (typeof legacyHostId !== "string") return true;
  const scoped = loadPairing(storage, legacyHostId);
  return scoped !== undefined && !samePairing(scoped, legacy);
}
