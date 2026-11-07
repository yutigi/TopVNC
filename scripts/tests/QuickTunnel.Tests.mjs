import test from "node:test";
import assert from "node:assert/strict";
import { connect } from "node:net";
import { once } from "node:events";
import { challengeResponse, startRfb } from "../lib/quick-tunnel-rfb.mjs";
import { tunnelArgs, isolatedEnvironment, isLoginResponse, authSessionMetadata, assessGate } from "../check-quick-tunnel.mjs";

test("a failed gate assessment preserves status diagnostics without response secrets", () => {
  const denied = { status: 302, location: "/login?state=SECRET", body: Buffer.from("SECRET"), contentType: "text/html" };
  const result = assessGate(denied, denied, { status: 101, body: Buffer.from("SECRET") }, "other hash");
  assert.deepEqual(result.statuses, { page: 302, asset: 302, websocket: 101, websocketHasLoginLocation: false });
  assert.equal(result.failure, "Unauthenticated WebSocket must require login");
  assert.doesNotMatch(JSON.stringify(result), /SECRET/);
  assert.equal(assessGate(denied, denied, denied, "other hash").failure, null);
  assert.notEqual(assessGate(denied, denied, { ...denied, status: 502 }, "other hash").failure, null);
});

test("expiry inspection selects the auth cookie and never exports token material", () => {
  const cookie = { name: "__Host-cloudflared-qt-auth-session", expires: 1200,
    value: Buffer.from(JSON.stringify({ nonce: "PRIVATE-NONCE", exp: 1200 })).toString("base64url") + ".PRIVATE-SIGNATURE" };
  const result = authSessionMetadata([{ name: "login-state", expires: 905, value: "SECRET" }, cookie], 900);
  assert.deepEqual(result, { authCookieCount: 1, browserExpiryAt: 1200,
    signedExpiryClaimAt: 1200, remainingSeconds: 300 });
  assert.doesNotMatch(JSON.stringify(result), /PRIVATE|SECRET/);
  assert.equal(authSessionMetadata([cookie, cookie]).signedExpiryClaimAt, null);
  assert.equal(authSessionMetadata([{ ...cookie, value: "garbage" }]).signedExpiryClaimAt, null);
  assert.equal(authSessionMetadata([{ name: "state", value: "secret", expires: 5 }]).remainingSeconds, null);
});

test("email-restricted launch keeps the explicit config and has no ungated retry", () => {
  assert.deepEqual(tunnelArgs("isolated.json", "127.0.0.1:6080", "owner@example.com"),
    ["tunnel", "--no-autoupdate", "--config", "isolated.json", "--url", "http://127.0.0.1:6080", "--allowed-mail", "owner@example.com"]);
  assert.equal(tunnelArgs("isolated.json", "127.0.0.1:6080").includes("--allowed-mail"), false);
  assert.deepEqual(isolatedEnvironment({ TUNNEL_URL: "elsewhere", tunnel_token: "private",
    TOPVNC_TEST_EMAIL: "owner@example.com", HTTPS_PROXY: "proxy", PATH: "tools" }),
    { HTTPS_PROXY: "proxy", PATH: "tools" });
});

test("a service failure, exposed TopVNC page, or WebSocket upgrade is never a login gate pass", () => {
  const response = (status, body = "", more = {}) => ({ status, body: Buffer.from(body), contentType: "text/html", ...more });
  for (const status of [101, 404, 429, 500, 502, 530]) assert.equal(isLoginResponse(response(status)), false);
  assert.equal(isLoginResponse(response(200, 'email <form id="connect-form">')), false);
  assert.equal(isLoginResponse(response(200, "Enter your email to sign in")), true);
  assert.equal(isLoginResponse(response(302, "", { location: "/login" })), true);
  assert.equal(isLoginResponse(response(403)), true);
});

test("VNC challenge response matches the known DES zero-key vector", () => {
  assert.equal(challengeResponse("\0".repeat(8), Buffer.alloc(16)).toString("hex"),
    "8ca64de9c1b123a78ca64de9c1b123a7");
});

test("the synthetic fixture requires VNC authentication and rejects a bad response", async () => {
  const fixture = await startRfb("probe123");
  const socket = connect({ host: "127.0.0.1", port: Number(fixture.address.split(":")[1]) });
  try {
    assert.equal((await once(socket, "data"))[0].toString(), "RFB 003.008\n");
    socket.write("RFB 003.008\n");
    assert.deepEqual((await once(socket, "data"))[0], Buffer.from([1, 2]));
    socket.write(Buffer.from([2]));
    assert.equal((await once(socket, "data"))[0].length, 16);
    socket.write(Buffer.alloc(16));
    assert.equal((await once(socket, "data"))[0].readUInt32BE(), 1);
    assert.equal(fixture.audit.rejected, 1);
    assert.equal(fixture.audit.authenticated, 0);
  } finally { socket.destroy(); await fixture.close(); }
});
