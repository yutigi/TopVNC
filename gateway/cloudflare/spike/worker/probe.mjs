// Synthetic transport diagnostics inside the browser-to-host E2E channel.
// No desktop or password is accessed. Cloudflare relays ciphertext but serves
// this code, so it is trusted not to substitute it (see the README).
import init, { E2eSession, fingerprint, newBrowserKey, parseInvite, publicKey } from "./build/browser-pkg/topvnc_spike_browser.js";
import { makePayload, verifyChunk } from "./probe-protocol.mjs";
import { forgetPairing, inviteFromHash, loadPairing, savePairing } from "./pairing-store.mjs";

const form = document.querySelector("form");
const start = form.querySelector("button[type=submit]");
const status = document.querySelector("#status");
const identity = document.querySelector("#identity");
const forget = document.querySelector("#forget");
const disconnect = document.querySelector("#disconnect");
const log = document.querySelector("#log");
const HINTS = {
  "browser key is not enrolled": "This browser is not enrolled on the host. Pair it again.",
  "no pairing invite is active": "The pairing invite expired or was already used. Run the gateway with --pair again.",
  "pairing invite expired or already used": "The pairing invite expired or was already used. Run the gateway with --pair again.",
  "E2E handshake failed": "The host rejected the encrypted handshake (pinned key or pairing secret mismatch).",
};
let storage;
try {
  storage = window.localStorage;
} catch {
  storage = undefined;
}
let pending; // A scanned invite plus a new browser key; memory only until verified.
let session;
let lines = [];

function report(message) {
  status.textContent = message;
  lines.push(`${new Date().toLocaleTimeString()} ${message}`);
  lines = lines.slice(-30);
  log.textContent = lines.join("\n");
}

function pairing() {
  return pending ?? loadPairing(storage);
}

function describeIdentity() {
  const current = pairing();
  start.disabled = Boolean(session) || !current;
  forget.disabled = Boolean(session) || !current;
  if (!current) {
    identity.textContent = "Not paired. On the host, start the gateway with --pair, sign in here, then scan its QR code with the camera.";
    return;
  }
  try {
    const host = fingerprint(current.host);
    const browser = fingerprint(publicKey(current.browser));
    identity.textContent = pending
      ? `Pairing invite for host ${host}. Start the test to enroll this browser (${browser}).`
      : `Paired with host ${host}. This browser: ${browser}.`;
  } catch {
    identity.textContent = "The saved pairing is invalid. Forget it and pair again.";
    start.disabled = true;
  }
}

function close(message) {
  if (!session) return;
  clearTimeout(session.timer);
  clearTimeout(session.next);
  session.socket.close(1000, "Probe stopped");
  session.e2e.free();
  session = undefined;
  disconnect.disabled = true;
  describeIdentity();
  report(message);
}

function sendSample(current) {
  if (session !== current) return;
  if (current.socket.bufferedAmount > 0) {
    close("Probe stopped: outgoing data is still queued.");
    return;
  }
  current.expected = makePayload(current.sequence);
  current.offset = 0;
  current.sentAt = performance.now();
  try {
    current.socket.send(current.e2e.seal(current.expected));
  } catch {
    close("Probe stopped: encryption failed.");
    return;
  }
  current.timer = setTimeout(() => close("Echo timed out. Reload if Access expired."), 5000);
}

function onHandshakeReply(current, reply) {
  clearTimeout(current.timer);
  try {
    current.socket.send(current.e2e.finish(reply));
  } catch {
    close("Host identity verification failed. No unencrypted fallback was attempted. Re-pair at the host only if you rotated its key.");
    return;
  }
  current.handshaking = false;
  report(`Encrypted channel to host ${fingerprint(current.pairing.host)} established. Sending one 1 KiB sample per second.`);
  sendSample(current);
}

function onSample(current, message) {
  try {
    current.offset = verifyChunk(current.expected, current.offset, current.e2e.open(message));
  } catch {
    close("Probe stopped: unexpected, altered or replayed encrypted data.");
    return;
  }
  if (current.offset !== current.expected.length) return;
  clearTimeout(current.timer);
  const elapsed = performance.now() - current.sentAt;
  current.expected = undefined;
  current.sequence += 1;
  if (current.sequence === 1 && pending) {
    // The host accepted the invite and echoed through the channel: keep the pairing.
    try {
      savePairing(storage, pending);
      pending = undefined;
      report("This browser is enrolled. The pairing is saved in this browser only.");
    } catch {
      report("Enrolled, but this browser could not save the pairing. Pair again next time.");
    }
  }
  if (current.sequence === 1 || current.sequence % 10 === 0) {
    report(`${current.sequence} encrypted samples verified. Latest echo round trip: ${elapsed.toFixed(1)} ms (not desktop latency).`);
  }
  current.next = setTimeout(() => sendSample(current), 1000);
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  if (session) return;
  if (location.protocol !== "https:") {
    report("Use the protected HTTPS workers.dev address.");
    return;
  }
  const keys = pairing();
  if (!keys) return;
  let e2e;
  try {
    e2e = new E2eSession(keys.browser, keys.host, pending?.secret);
  } catch {
    report("The saved pairing is invalid. Forget it and pair again.");
    return;
  }
  const socket = new WebSocket(`wss://${location.host}/ws`, ["binary"]);
  socket.binaryType = "arraybuffer";
  const current = { socket, e2e, pairing: keys, handshaking: true, sequence: 0, offset: 0 };
  session = current;
  disconnect.disabled = false;
  describeIdentity();
  current.timer = setTimeout(() => close("Connection timed out."), 10000);
  report(pending ? "Connecting to enroll this browser…" : "Connecting to the synthetic echo target…");
  socket.addEventListener("open", () => {
    if (session !== current) return;
    if (socket.protocol !== "binary") {
      close("Probe stopped: binary subprotocol was not negotiated.");
      return;
    }
    socket.send(e2e.firstMessage());
  });
  socket.addEventListener("message", (event) => {
    if (session !== current) return;
    if (!(event.data instanceof ArrayBuffer)) {
      close("Probe stopped: binary data required.");
      return;
    }
    const message = new Uint8Array(event.data);
    if (current.handshaking) onHandshakeReply(current, message);
    else onSample(current, message);
  });
  socket.addEventListener("close", (event) => {
    if (session !== current) return;
    const hint = Object.hasOwn(HINTS, event.reason) ? HINTS[event.reason]
      : event.reason ? `Host: ${event.reason}.` : "Reload to sign in if Access expired.";
    close(`Disconnected (${event.code}). ${hint}`);
  });
  socket.addEventListener("error", () => {
    if (session === current) close("Connection failed. Check Access and the private gateway.");
  });
});
disconnect.addEventListener("click", () => close("Probe stopped."));
forget.addEventListener("click", () => {
  if (session) return;
  forgetPairing(storage);
  pending = undefined;
  describeIdentity();
  report("Pairing removed from this browser. The host still lists its key until you remove it there.");
});
window.addEventListener("pagehide", () => close("Probe stopped."));

async function main() {
  const invite = inviteFromHash(location.hash);
  // Remove the one-time secret from the address bar and this history entry.
  if (location.hash) history.replaceState(null, "", location.pathname + location.search);
  await init({ module_or_path: "/topvnc_spike_browser_bg.wasm" });
  if (invite) {
    try {
      const parsed = parseInvite(invite);
      pending = { host: parsed.host, secret: parsed.secret, browser: newBrowserKey() };
      parsed.free();
      report("Pairing invite read. Compare the host fingerprint with the host console, then start the test.");
    } catch {
      report("The pairing link is invalid. Scan the host's QR code again.");
    }
  }
  describeIdentity();
}

main().catch(() => report("The encryption module failed to load. Desktop and test connections stay disabled."));
