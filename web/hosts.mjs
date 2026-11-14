import { HOST_ID_PATTERN } from "./pairing-store.mjs";

export function validateHosts(value) {
  if (!value || typeof value.legacy !== "boolean" || !Array.isArray(value.hosts)
    || value.hosts.length < 1 || value.hosts.length > 32) throw new Error("Invalid machine registry");
  const ids = new Set();
  for (const host of value.hosts) {
    if (!host || typeof host.id !== "string" || !HOST_ID_PATTERN.test(host.id) || ids.has(host.id)
      || typeof host.name !== "string" || !host.name.trim() || host.name.length > 80) throw new Error("Invalid machine entry");
    ids.add(host.id);
  }
  if (value.legacyHostId !== null && !ids.has(value.legacyHostId)) throw new Error("Invalid migration target");
  if (value.legacy && (value.hosts.length !== 1 || value.hosts[0].id !== "default" || value.legacyHostId !== null)) {
    throw new Error("Invalid legacy registry");
  }
  return value;
}

export const legacyHosts = () => ({ legacy: true, legacyHostId: null, hosts: [{ id: "default", name: "This machine" }] });

export async function fetchHosts(fetcher, local = false) {
  const response = await fetcher("/api/hosts", { redirect: "manual", cache: "no-store" });
  if (local && response.status === 404) return legacyHosts();
  if (!response.ok || Number(response.headers.get("Content-Length")) > 16384 || !response.body) {
    throw new Error("Machine list unavailable");
  }
  const reader = response.body.getReader();
  const chunks = [];
  let length = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    length += value.length;
    if (length > 16384) { await reader.cancel(); throw new Error("Machine list too large"); }
    chunks.push(value);
  }
  const bytes = new Uint8Array(length);
  let offset = 0;
  for (const chunk of chunks) { bytes.set(chunk, offset); offset += chunk.length; }
  return validateHosts(JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes)));
}

// A selector in the URL is never a destination. Even invites must refer to an
// ID in the authenticated registry. Unscoped invites are only for legacy mode.
export function inviteTarget(registry, search) {
  const params = new URLSearchParams(search);
  const ids = params.getAll("host");
  if (ids.length === 0 && registry.legacy) return registry.hosts[0].id;
  if (ids.length !== 1 || !registry.hosts.some((host) => host.id === ids[0])) throw new Error("Pairing link must name a configured machine");
  return ids[0];
}

export function invitePolicy(saved, hostKey) {
  if (!saved) return "accept";
  return saved.host === hostKey ? "already-paired" : "changed-key";
}

export function socketPath(registry, id) {
  if (!registry.hosts.some((host) => host.id === id)) throw new Error("Unknown machine");
  return registry.legacy ? "/ws" : `/ws/${id}`;
}
