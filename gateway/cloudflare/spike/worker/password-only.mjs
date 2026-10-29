// Test-harness adapter for pinned noVNC 1.7.0, not a production protocol API.
// Refuse None and nested security schemes, including a server-selected RFB 3.3 None.
export function passwordOnlyRfb(BaseRfb) {
  for (const name of ["_isSupportedSecurityType", "_negotiateAuthentication"]) {
    if (typeof BaseRfb.prototype[name] !== "function") {
      throw new Error("The pinned noVNC authentication interface changed; review the harness.");
    }
  }
  return class extends BaseRfb {
    _isSupportedSecurityType(type) {
      return type === 2;
    }

    _negotiateAuthentication() {
      if (this._rfbAuthScheme !== 2) {
        return this._fail("This connection test requires VNC password authentication.");
      }
      return super._negotiateAuthentication();
    }
  };
}
