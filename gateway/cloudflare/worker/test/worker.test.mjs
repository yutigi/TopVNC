import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { afterEach, beforeEach, describe, it } from "node:test";
import worker, {
  AccessPolicy,
  KeySet,
  MAX_JWKS_BYTES,
  MAX_TOKEN_BYTES,
  base64UrlDecode,
  resetKeyCache,
  tokenKeyId,
  validateUpgrade,
  validateUpstream,
} from "../src/index.mjs";

// Signed by crates/topvnc-access's fixtures; its test keeps this file current.
const vectors = JSON.parse(readFileSync(new URL("./access-vectors.json", import.meta.url), "utf8"));
const policy = new AccessPolicy(vectors.issuer, vectors.audience, vectors.owner_email);
const b64 = (value) => Buffer.from(typeof value === "string" ? value : JSON.stringify(value)).toString("base64url");
const jwksBytes = (value) => new TextEncoder().encode(typeof value === "string" ? value : JSON.stringify(value));
const fixtureKey = JSON.parse(vectors.jwks).keys[0];

describe("JWT verification against the Rust fixtures", () => {
  it("accepts every token the Rust crate accepts", async () => {
    const keys = await KeySet.parse(jwksBytes(vectors.jwks));
    for (const [name, token] of Object.entries(vectors.accepted)) {
      assert.deepEqual(await keys.verify(token, policy, vectors.now), { expiresAt: vectors.now + 300 }, name);
    }
  });

  it("rejects every token the Rust crate rejects", async () => {
    const keys = await KeySet.parse(jwksBytes(vectors.jwks));
    for (const [name, token] of Object.entries(vectors.rejected)) {
      await assert.rejects(keys.verify(token, policy, vectors.now), undefined, name);
    }
  });

  it("rejects a token at its expiry and a different audience or owner", async () => {
    const keys = await KeySet.parse(jwksBytes(vectors.jwks));
    const token = vectors.accepted.array_audience;
    await assert.rejects(keys.verify(token, policy, vectors.now + 300));
    await assert.rejects(keys.verify(token, new AccessPolicy(vectors.issuer, "different-app", vectors.owner_email), vectors.now));
    await assert.rejects(keys.verify(token, new AccessPolicy(vectors.issuer, vectors.audience, "other@example.com"), vectors.now));
  });

  it("rejects missing, malformed, and oversized tokens", async () => {
    const keys = await KeySet.parse(jwksBytes(vectors.jwks));
    for (const token of ["", "e30.e30.e30", "x".repeat(MAX_TOKEN_BYTES + 1), "a.b", "a..c", "a.b.c.d"]) {
      await assert.rejects(keys.verify(token, policy, vectors.now));
    }
  });

  it("rejects unknown algorithms and critical extensions", () => {
    for (const header of [
      { alg: "none", kid: "test-key" },
      { alg: "HS256", kid: "test-key" },
      { alg: "RS256", kid: "test-key", crit: ["unsupported"] },
      { alg: "RS256", kid: "test-key", b64: false },
      { alg: "RS256", kid: "" },
      { alg: "RS256" },
    ]) {
      assert.throws(() => tokenKeyId(`${b64(header)}.e30.AA`), undefined, JSON.stringify(header));
    }
    assert.equal(tokenKeyId(`${b64({ alg: "RS256", kid: "test-key", b64: true })}.e30.AA`), "test-key");
  });
});

describe("signing key sets", () => {
  it("rejects oversized, empty, malformed, and duplicate key sets", async () => {
    await assert.rejects(KeySet.parse(new Uint8Array(MAX_JWKS_BYTES + 1).fill(32)));
    for (const jwks of [
      "not json",
      { keys: [] },
      { keys: Array(9).fill(fixtureKey) },
      { keys: [fixtureKey, fixtureKey] },
      { keys: [{ ...fixtureKey, n: 7 }] },
      { keys: [{ ...fixtureKey, e: "AAAB" }] },
      { keys: [{ ...fixtureKey, n: fixtureKey.n.slice(0, 200) }] },
      { keys: [{ ...fixtureKey, kty: "EC" }] },
      { keys: [{ ...fixtureKey, use: "enc" }] },
    ]) {
      await assert.rejects(KeySet.parse(jwksBytes(jwks)), undefined, JSON.stringify(jwks).slice(0, 80));
    }
  });

  it("skips keys for other algorithms but keeps usable ones", async () => {
    const keys = await KeySet.parse(jwksBytes({ keys: [{ ...fixtureKey, kid: "ec", kty: "EC" }, fixtureKey] }));
    assert.ok(keys.has("test-key"));
    assert.ok(!keys.has("ec"));
  });
});

describe("request policy", () => {
  const ok = { method: "GET", path: "/ws", hasQuery: false, origin: vectors.origin, protocols: "binary", upgrade: "websocket" };

  it("rejects query strings, wrong origins, and non-binary upgrades", () => {
    validateUpgrade(ok, vectors.origin);
    for (const bad of [
      { hasQuery: true },
      { origin: "https://attacker.example" },
      { origin: undefined },
      { method: "POST" },
      { path: "/admin" },
      { protocols: "text" },
      { upgrade: "h2c" },
    ]) {
      assert.throws(() => validateUpgrade({ ...ok, ...bad }, vectors.origin), undefined, JSON.stringify(bad));
    }
    assert.throws(() => validateUpgrade({ ...ok, origin: "https://example.com" }, "https://example.com"));
  });

  it("restricts the upstream to one private HTTP /ws endpoint", () => {
    for (const valid of ["http://127.0.0.1:6080/ws", "http://10.0.0.2:6080/ws", "http://[::1]:6080/ws", "http://[fd00::1]:6080/ws"]) {
      validateUpstream(valid);
    }
    for (const invalid of [
      "http://example.com:6080/ws",
      "http://8.8.8.8:6080/ws",
      "http://127.0.0.1:6080/ws?target=x",
      "http://user:pass@127.0.0.1:6080/ws",
      "http://127.0.0.1:6080/admin",
      "http://127.0.0.1/ws",
      "https://127.0.0.1:6080/ws",
      "http://[2001:db8::1]:6080/ws",
    ]) {
      assert.throws(() => validateUpstream(invalid), undefined, invalid);
    }
  });

  it("accepts only an exact Access team issuer", () => {
    assert.equal(new AccessPolicy(`${vectors.issuer}/`, "a", "o@x").issuer, vectors.issuer);
    for (const issuer of [
      "http://team.cloudflareaccess.com",
      "https://cloudflareaccess.com",
      "https://a.b.cloudflareaccess.com",
      "https://team.cloudflareaccess.com:8443",
      "https://team.cloudflareaccess.com/path",
      "https://team.cloudflareaccess.com?x",
      "https://team.example.com",
    ]) {
      assert.throws(() => new AccessPolicy(issuer, "a", "o@x"), undefined, issuer);
    }
    assert.throws(() => new AccessPolicy(vectors.issuer, "", "o@x"));
    assert.throws(() => new AccessPolicy(vectors.issuer, "a", "no-at-sign"));
    assert.throws(() => new AccessPolicy(vectors.issuer, "a", "o @x"));
  });

  it("decodes only canonical unpadded base64url", () => {
    assert.deepEqual([...base64UrlDecode("AQID")], [1, 2, 3]);
    for (const bad of ["AQ==", "A", "AQ+/", "AR"]) {
      assert.throws(() => base64UrlDecode(bad), undefined, bad);
    }
  });
});

describe("fetch handler", () => {
  const env = (hostNet) => ({
    APP_ORIGIN: vectors.origin,
    ACCESS_ISSUER: vectors.issuer,
    ACCESS_AUD: vectors.audience,
    OWNER_EMAIL: vectors.owner_email,
    UPSTREAM_URL: "http://127.0.0.1:6080/ws",
    HOST_NET: hostNet,
  });
  const upgrade = (token, extra = {}) =>
    new Request(`${vectors.origin}/ws`, {
      headers: {
        Origin: vectors.origin,
        Upgrade: "websocket",
        "Sec-WebSocket-Protocol": "binary",
        ...(token === undefined ? {} : { "Cf-Access-Jwt-Assertion": token }),
        ...extra,
      },
    });
  const realFetch = globalThis.fetch;
  const realNow = Date.now;
  let jwksFetches;
  let errors;
  const realError = console.error;

  beforeEach(() => {
    resetKeyCache();
    jwksFetches = [];
    errors = [];
    console.error = (message) => errors.push(message);
    Date.now = () => vectors.now * 1000;
    globalThis.fetch = async (url, init) => {
      jwksFetches.push({ url: String(url), redirect: init?.redirect });
      return new Response(vectors.jwks, { status: 200 });
    };
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    Date.now = realNow;
    console.error = realError;
  });

  it("forwards an owner's upgrade to the private gateway unchanged", async () => {
    const sent = [];
    const upgraded = { status: 101, webSocket: {} };
    const response = await worker.fetch(
      upgrade(vectors.accepted.array_audience),
      env({ fetch: async (request) => (sent.push(request), upgraded) }),
    );
    assert.equal(response, upgraded);
    assert.equal(sent.length, 1);
    assert.equal(sent[0].url, "http://127.0.0.1:6080/ws");
    assert.equal(sent[0].headers.get("Cf-Access-Jwt-Assertion"), vectors.accepted.array_audience);
    assert.equal(sent[0].headers.get("Origin"), vectors.origin);
    assert.equal(sent[0].headers.get("Sec-WebSocket-Protocol"), "binary");
    assert.deepEqual(jwksFetches, [{ url: `${vectors.issuer}/cdn-cgi/access/certs`, redirect: "manual" }]);
  });

  it("reuses cached signing keys for later upgrades", async () => {
    const hostNet = { fetch: async () => ({ status: 101 }) };
    await worker.fetch(upgrade(vectors.accepted.array_audience), env(hostNet));
    await worker.fetch(upgrade(vectors.accepted.single_audience), env(hostNet));
    assert.equal(jwksFetches.length, 1);
  });

  it("never contacts the gateway for rejected requests", async () => {
    let contacted = false;
    const hostNet = { fetch: async () => ((contacted = true), { status: 101 }) };
    const cases = [
      [new Request(`${vectors.origin}/other`), 404],
      [upgrade(vectors.accepted.array_audience, { Origin: "https://attacker.example" }), 403],
      [new Request(`${vectors.origin}/ws?target=127.0.0.1:22`, { headers: { Origin: vectors.origin, Upgrade: "websocket", "Sec-WebSocket-Protocol": "binary" } }), 403],
      [new Request(`https://other.workers.dev/ws`, { headers: { Origin: vectors.origin, Upgrade: "websocket", "Sec-WebSocket-Protocol": "binary" } }), 403],
      [upgrade(undefined), 401],
      [upgrade("not-a-jwt"), 401],
      [upgrade(vectors.rejected.wrong_email), 401],
      [upgrade(vectors.rejected.forged_payload), 401],
    ];
    for (const [request, status] of cases) {
      resetKeyCache();
      const response = await worker.fetch(request, env(hostNet));
      assert.equal(response.status, status, request.url);
    }
    assert.equal(contacted, false);
  });

  it("rejects an unknown key and rate-limits the refresh it triggers", async () => {
    const hostNet = { fetch: async () => ({ status: 101 }) };
    assert.equal((await worker.fetch(upgrade(vectors.rejected.unknown_key), env(hostNet))).status, 401);
    // The cached set lacks the key, but a refresh within 10 s is refused.
    assert.equal((await worker.fetch(upgrade(vectors.rejected.unknown_key), env(hostNet))).status, 503);
    assert.equal(jwksFetches.length, 1);
    assert.deepEqual(errors, ["WebSocket upgrade failed; inspect configuration and private connectivity"]);
    assert.ok(!errors.join("").includes(vectors.rejected.unknown_key));
  });

  it("rejects oversized or failed signing key responses", async () => {
    const hostNet = { fetch: async () => ({ status: 101 }) };
    globalThis.fetch = async () => new Response("x".repeat(MAX_JWKS_BYTES + 1), { status: 200 });
    assert.equal((await worker.fetch(upgrade(vectors.accepted.array_audience), env(hostNet))).status, 503);
    resetKeyCache();
    globalThis.fetch = async () => new Response("", { status: 302, headers: { Location: "https://evil.example" } });
    assert.equal((await worker.fetch(upgrade(vectors.accepted.array_audience), env(hostNet))).status, 503);
  });

  it("maps a refused upgrade to 502 and a misconfiguration to 503", async () => {
    const token = vectors.accepted.array_audience;
    assert.equal((await worker.fetch(upgrade(token), env({ fetch: async () => ({ status: 403 }) }))).status, 502);
    assert.equal((await worker.fetch(upgrade(token), env(undefined))).status, 503);
    assert.equal((await worker.fetch(upgrade(token), { ...env({ fetch: async () => ({ status: 101 }) }), UPSTREAM_URL: "http://8.8.8.8:6080/ws" })).status, 503);
    assert.equal((await worker.fetch(upgrade(token), { ...env({ fetch: async () => ({ status: 101 }) }), ACCESS_ISSUER: "https://evil.example" })).status, 503);
  });
});
