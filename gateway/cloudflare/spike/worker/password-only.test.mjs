import { test } from "node:test";
import assert from "node:assert/strict";
import { passwordOnlyRfb } from "./password-only.mjs";

class Handshake {
  _isSupportedSecurityType() { return true; }
  _negotiateAuthentication() { this.startedAuth = true; return true; }
  _fail() { this.failed = true; return false; }
}

test("rejects offered None/nested schemes and selects VNC password auth", () => {
  const rfb = new (passwordOnlyRfb(Handshake))();
  for (const type of [1, 6, 16, 19, 22, 30, 113, 256]) {
    assert.equal(rfb._isSupportedSecurityType(type), false);
  }
  assert.equal(rfb._isSupportedSecurityType(2), true);
});

test("rejects server-selected unauthenticated RFB before authentication proceeds", () => {
  const rfb = new (passwordOnlyRfb(Handshake))();
  rfb._rfbAuthScheme = 1;
  assert.equal(rfb._negotiateAuthentication(), false);
  assert.equal(rfb.failed, true);
  assert.equal(rfb.startedAuth, undefined);
});

test("password authentication proceeds and an incompatible dependency fails closed", () => {
  const rfb = new (passwordOnlyRfb(Handshake))();
  rfb._rfbAuthScheme = 2;
  assert.equal(rfb._negotiateAuthentication(), true);
  assert.equal(rfb.startedAuth, true);
  assert.throws(() => passwordOnlyRfb(class {}), /interface changed/);
});
