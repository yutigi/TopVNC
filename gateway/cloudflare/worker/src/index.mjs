// Cloudflare Worker for TopVNC's browser client. Static assets (web/dist) are
// served before this code runs; it handles only `/ws`: it verifies the
// Cloudflare Access JWT, then forwards the WebSocket upgrade unchanged to the
// one private gateway behind the Workers VPC binding. It never accepts the
// socket, reads messages, or chooses a destination from the request.
//
// This is plain JavaScript with WebCrypto rather than Rust compiled to
// WebAssembly. A cold isolate spent most of its CPU instantiating and
// lazily compiling the WebAssembly module and running RSA in it, which pushed
// first upgrades past the Workers Free 10 ms limit. The checks mirror
// crates/topvnc-access, and the private gateway repeats every one of them
// with that crate before it opens a session. test/access-vectors.json holds
// tokens signed by the Rust fixtures so both implementations are tested
// against the same inputs.

export const MAX_TOKEN_BYTES = 16 * 1024;
export const MAX_JWKS_BYTES = 64 * 1024;
export const KEY_TTL_SECONDS = 300;
export const KEY_REFRESH_SECONDS = 10;
const UPGRADE_TIMEOUT_MS = 5000;
const JWKS_TIMEOUT_MS = 5000;
const RS256 = { name: "RSASSA-PKCS1-v1_5", hash: "SHA-256" };

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const LOOKUP = new Int16Array(128).fill(-1);
for (let i = 0; i < ALPHABET.length; i++) LOOKUP[ALPHABET.charCodeAt(i)] = i;
const utf8 = new TextDecoder("utf-8", { fatal: true });
const encoder = new TextEncoder();

class PolicyError extends Error {}

function reject(message) {
  throw new PolicyError(message);
}

/** Strict unpadded base64url, rejecting non-canonical trailing bits like Rust's base64 crate. */
export function base64UrlDecode(text) {
  if (typeof text !== "string" || text.length % 4 === 1) reject("invalid base64url");
  const out = new Uint8Array(Math.floor((text.length * 3) / 4));
  let buffer = 0;
  let bits = 0;
  let o = 0;
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    const v = c < 128 ? LOOKUP[c] : -1;
    if (v < 0) reject("invalid base64url");
    buffer = (buffer << 6) | v;
    bits += 6;
    if (bits >= 8) {
      bits -= 8;
      out[o++] = (buffer >> bits) & 0xff;
    }
    buffer &= (1 << bits) - 1;
  }
  if (buffer !== 0) reject("invalid base64url");
  return out;
}

function decodeJson(segment, message) {
  try {
    return JSON.parse(utf8.decode(base64UrlDecode(segment)));
  } catch {
    reject(message);
  }
}

const isObject = (v) => typeof v === "object" && v !== null && !Array.isArray(v);
const isAbsent = (v) => v === undefined || v === null;
const isU64 = (v) => Number.isSafeInteger(v) && v >= 0;
const byteLength = (s) => encoder.encode(s).length;
const asciiLower = (s) => s.replace(/[A-Z]/g, (c) => c.toLowerCase());

function parseUrl(text, message) {
  try {
    return new URL(text);
  } catch {
    reject(message);
  }
}

export class AccessPolicy {
  constructor(issuer, audience, ownerEmail) {
    issuer = String(issuer).replace(/\/+$/, "");
    const url = parseUrl(issuer, "invalid Access issuer");
    const team = url.hostname.endsWith(".cloudflareaccess.com")
      ? url.hostname.slice(0, -".cloudflareaccess.com".length)
      : reject("invalid Access team hostname");
    if (
      url.protocol !== "https:" ||
      team === "" ||
      team.includes(".") ||
      url.username !== "" ||
      url.password !== "" ||
      url.port !== "" ||
      url.pathname !== "/" ||
      issuer.includes("?") ||
      issuer.includes("#") ||
      typeof audience !== "string" ||
      audience === "" ||
      byteLength(audience) > 256 ||
      typeof ownerEmail !== "string" ||
      !ownerEmail.includes("@") ||
      byteLength(ownerEmail) > 254 ||
      /\s/u.test(ownerEmail)
    ) {
      reject("invalid Access configuration");
    }
    this.issuer = issuer;
    this.audience = audience;
    this.ownerEmail = ownerEmail;
  }

  get jwksUrl() {
    return `${this.issuer}/cdn-cgi/access/certs`;
  }
}

function tokenParts(token) {
  if (typeof token !== "string" || token === "" || token.length > MAX_TOKEN_BYTES) {
    reject("invalid token size");
  }
  const parts = token.split(".");
  if (parts.length !== 3) reject("invalid JWT structure");
  if (parts.some((p) => p === "")) reject("empty JWT component");
  return parts;
}

export function tokenKeyId(token) {
  const [header] = tokenParts(token);
  if (header.length > 1024) reject("JWT header too large");
  const h = decodeJson(header, "invalid JWT header");
  if (!isObject(h) || typeof h.alg !== "string" || typeof h.kid !== "string") {
    reject("invalid JWT header");
  }
  if (
    h.alg !== "RS256" ||
    h.kid === "" ||
    h.kid.length > 128 ||
    !isAbsent(h.crit) ||
    !(isAbsent(h.b64) || h.b64 === true)
  ) {
    reject("unsupported JWT algorithm or key");
  }
  return h.kid;
}

function bitLength(bytes) {
  let i = 0;
  while (i < bytes.length && bytes[i] === 0) i++;
  if (i === bytes.length) return 0;
  return (bytes.length - i - 1) * 8 + (32 - Math.clz32(bytes[i]));
}

const optionalString = (v) => isAbsent(v) || typeof v === "string";

export class KeySet {
  constructor(keys) {
    this.keys = keys;
  }

  static async parse(bytes) {
    if (bytes.length > MAX_JWKS_BYTES) reject("JWKS too large");
    let jwks;
    try {
      jwks = JSON.parse(utf8.decode(bytes));
    } catch {
      reject("invalid JWKS");
    }
    if (
      !isObject(jwks) ||
      !Array.isArray(jwks.keys) ||
      !jwks.keys.every(
        (k) =>
          isObject(k) &&
          typeof k.kid === "string" &&
          typeof k.kty === "string" &&
          optionalString(k.alg) &&
          optionalString(k.use) &&
          typeof k.n === "string" &&
          typeof k.e === "string",
      )
    ) {
      reject("invalid JWKS");
    }
    if (jwks.keys.length === 0 || jwks.keys.length > 8) reject("invalid JWKS key count");
    const keys = new Map();
    for (const key of jwks.keys) {
      if (
        key.kty !== "RSA" ||
        (!isAbsent(key.alg) && key.alg !== "RS256") ||
        (!isAbsent(key.use) && key.use !== "sig")
      ) {
        continue;
      }
      if (
        key.kid === "" ||
        key.kid.length > 128 ||
        key.n.length > 684 ||
        key.e.length > 12 ||
        keys.has(key.kid)
      ) {
        reject("invalid JWK metadata");
      }
      const n = base64UrlDecode(key.n);
      const e = base64UrlDecode(key.e);
      const bits = bitLength(n);
      if (bits < 2048 || bits > 4096 || e.length === 0 || e.length > 4) reject("invalid RSA key size");
      if (bitLength(e) < 2) reject("invalid RSA public key");
      let cryptoKey;
      try {
        cryptoKey = await crypto.subtle.importKey(
          "jwk",
          { kty: "RSA", n: key.n, e: key.e, alg: "RS256", ext: true },
          RS256,
          false,
          ["verify"],
        );
      } catch {
        reject("invalid RSA public key");
      }
      keys.set(key.kid, cryptoKey);
    }
    if (keys.size === 0) reject("no usable signing keys");
    return new KeySet(keys);
  }

  has(kid) {
    return this.keys.has(kid);
  }

  /** Resolves to { expiresAt } for a valid owner token; rejects otherwise. */
  async verify(token, policy, now) {
    const kid = tokenKeyId(token);
    const [, payload, signature] = tokenParts(token);
    const key = this.keys.get(kid) ?? reject("unknown signing key");
    const sig = base64UrlDecode(signature);
    const signed = encoder.encode(token.slice(0, token.lastIndexOf(".")));
    if (!(await crypto.subtle.verify(RS256, key, sig, signed))) reject("JWT signature rejected");
    const c = decodeJson(payload, "invalid JWT claims");
    const audiences = typeof c?.aud === "string" ? [c.aud] : c?.aud;
    if (
      !isObject(c) ||
      typeof c.iss !== "string" ||
      !Array.isArray(audiences) ||
      !audiences.every((a) => typeof a === "string") ||
      !isU64(c.exp) ||
      !(isAbsent(c.nbf) || isU64(c.nbf)) ||
      typeof c.email !== "string"
    ) {
      reject("invalid JWT claims");
    }
    const audienceMatches =
      (typeof c.aud === "string" || audiences.length <= 16) && audiences.includes(policy.audience);
    if (
      c.iss !== policy.issuer ||
      !audienceMatches ||
      c.exp <= now ||
      (!isAbsent(c.nbf) && c.nbf > now) ||
      asciiLower(c.email) !== asciiLower(policy.ownerEmail)
    ) {
      reject("JWT claims rejected");
    }
    return { expiresAt: c.exp };
  }
}

export function validateOrigin(origin) {
  const url = parseUrl(origin, "invalid app origin");
  if (url.protocol !== "https:" || !url.hostname.endsWith(".workers.dev") || url.origin !== origin) {
    reject("app origin must be an exact HTTPS workers.dev origin");
  }
}

export function validateUpgrade({ method, path, hasQuery, origin, protocols, upgrade }, appOrigin) {
  validateOrigin(appOrigin);
  if (method !== "GET" || path !== "/ws" || hasQuery) reject("invalid upgrade route");
  if (origin !== appOrigin) reject("Origin rejected");
  if (
    !(typeof protocols === "string" && protocols.length <= 128 && protocols.split(",").some((v) => v.trim() === "binary")) ||
    !(typeof upgrade === "string" && asciiLower(upgrade) === "websocket")
  ) {
    reject("binary WebSocket upgrade required");
  }
}

function isPrivateIp(host) {
  const v4 = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (v4) {
    const [a, b] = [Number(v4[1]), Number(v4[2])];
    return a === 127 || a === 10 || (a === 172 && b >= 16 && b <= 31) || (a === 192 && b === 168);
  }
  if (host.startsWith("[") && host.endsWith("]")) {
    // WHATWG URL serializes IPv6 in lowercase compressed form.
    const ip = host.slice(1, -1);
    return ip === "::1" || /^f[cd][0-9a-f]{2}:/.test(ip);
  }
  return reject("upstream must use a private IP literal");
}

export function validateUpstream(upstream) {
  const url = parseUrl(upstream, "invalid upstream URL");
  if (
    !isPrivateIp(url.hostname) ||
    url.protocol !== "http:" ||
    url.username !== "" ||
    url.password !== "" ||
    upstream.includes("@") ||
    url.port === "" ||
    url.pathname !== "/ws" ||
    upstream.includes("?") ||
    upstream.includes("#")
  ) {
    reject("upstream must be one fixed private HTTP /ws endpoint");
  }
}

// Per-isolate signing key cache, like the gateway's.
const cache = { keys: null, loadedAt: 0, lastAttempt: null };
const nowSeconds = () => Math.floor(Date.now() / 1000);
const since = (time, then) => Math.max(0, time - then);

/** Test hook: forget cached signing keys. */
export function resetKeyCache() {
  cache.keys = null;
  cache.loadedAt = 0;
  cache.lastAttempt = null;
}

async function readBounded(response) {
  const length = Number(response.headers.get("Content-Length"));
  if (Number.isFinite(length) && length > MAX_JWKS_BYTES) reject("JWKS too large");
  if (!response.body) return new Uint8Array();
  const chunks = [];
  let total = 0;
  const reader = response.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    total += value.length;
    if (total > MAX_JWKS_BYTES) {
      await reader.cancel();
      reject("JWKS too large");
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.length;
  }
  return bytes;
}

async function signingKeys(policy, kid) {
  const time = nowSeconds();
  if (cache.keys && since(time, cache.loadedAt) < KEY_TTL_SECONDS && cache.keys.has(kid)) {
    return cache.keys;
  }
  if (cache.lastAttempt !== null && since(time, cache.lastAttempt) < KEY_REFRESH_SECONDS) {
    throw new Error("Signing key refresh rate limited");
  }
  cache.lastAttempt = time;
  const response = await fetch(policy.jwksUrl, {
    redirect: "manual",
    signal: AbortSignal.timeout(JWKS_TIMEOUT_MS),
  });
  if (response.status !== 200) throw new Error("Signing keys unavailable");
  const keys = await KeySet.parse(await readBounded(response));
  cache.keys = keys;
  cache.loadedAt = time;
  return keys;
}

const text = (body, status) => new Response(body, { status });

async function handle(request, env) {
  const url = new URL(request.url);
  if (url.pathname !== "/ws") return text("Not found", 404);
  const appOrigin = String(env.APP_ORIGIN);
  const headers = request.headers;
  try {
    if (url.origin !== appOrigin) reject("wrong host");
    validateUpgrade(
      {
        method: request.method,
        path: url.pathname,
        hasQuery: url.search !== "" || request.url.includes("?"),
        origin: headers.get("Origin") ?? undefined,
        protocols: headers.get("Sec-WebSocket-Protocol") ?? undefined,
        upgrade: headers.get("Upgrade") ?? undefined,
      },
      appOrigin,
    );
  } catch (error) {
    if (error instanceof PolicyError) return text("WebSocket request rejected", 403);
    throw error;
  }
  const token = headers.get("Cf-Access-Jwt-Assertion");
  if (token === null) return text("Access authentication required", 401);
  const policy = new AccessPolicy(env.ACCESS_ISSUER, env.ACCESS_AUD, env.OWNER_EMAIL);
  let kid;
  try {
    kid = tokenKeyId(token);
  } catch {
    return text("Access authentication failed", 401);
  }
  const keys = await signingKeys(policy, kid);
  try {
    await keys.verify(token, policy, nowSeconds());
  } catch {
    return text("Access authentication failed", 401);
  }
  const upstream = String(env.UPSTREAM_URL);
  validateUpstream(upstream);
  const binding = env.HOST_NET;
  if (typeof binding?.fetch !== "function") throw new Error("HOST_NET has no fetch API");
  // Abort only a pending handshake; the timer is cleared once the gateway answers.
  const controller = new AbortController();
  const outbound = new Request(upstream, {
    method: "GET",
    headers: {
      Upgrade: "websocket",
      "Sec-WebSocket-Protocol": "binary",
      Origin: appOrigin,
      "Cf-Access-Jwt-Assertion": token,
    },
    redirect: "manual",
    signal: controller.signal,
  });
  let timer;
  const deadline = new Promise((_, fail) => {
    timer = setTimeout(() => {
      controller.abort();
      fail(new Error("Private upgrade timed out"));
    }, UPGRADE_TIMEOUT_MS);
  });
  let response;
  try {
    response = await Promise.race([binding.fetch(outbound), deadline]);
  } finally {
    clearTimeout(timer);
  }
  if (response.status !== 101) return text("Private gateway refused upgrade", 502);
  // Return the runtime's WebSocket response untouched: no accept() or message loop.
  return response;
}

export default {
  async fetch(request, env) {
    try {
      return await handle(request, env);
    } catch {
      // Never log request headers, JWTs, upstream response bodies, or credentials.
      console.error("WebSocket upgrade failed; inspect configuration and private connectivity");
      return text("Private gateway unavailable", 503);
    }
  },
};
