// Synthetic transport diagnostics only. No desktop or password is accessed.
import { makePayload, verifyChunk } from "./probe-protocol.mjs";

const form = document.querySelector("form");
const status = document.querySelector("#status");
const disconnect = document.querySelector("#disconnect");
const log = document.querySelector("#log");
let session;
let lines = [];

function report(message) {
  status.textContent = message;
  lines.push(`${new Date().toLocaleTimeString()} ${message}`);
  lines = lines.slice(-30);
  log.textContent = lines.join("\n");
}

function close(message) {
  if (!session) return;
  clearTimeout(session.timer);
  clearTimeout(session.next);
  session.socket.close(1000, "Probe stopped");
  session = undefined;
  disconnect.disabled = true;
  form.querySelector("button").disabled = false;
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
  current.socket.send(current.expected);
  current.timer = setTimeout(() => close("Echo timed out. Reload if Access expired."), 5000);
}

form.addEventListener("submit", (event) => {
  event.preventDefault();
  if (session) return;
  if (location.protocol !== "https:") {
    report("Use the protected HTTPS workers.dev address.");
    return;
  }
  const socket = new WebSocket(`wss://${location.host}/ws`, ["binary"]);
  socket.binaryType = "arraybuffer";
  const current = { socket, sequence: 0, offset: 0 };
  session = current;
  disconnect.disabled = false;
  form.querySelector("button").disabled = true;
  current.timer = setTimeout(() => close("Connection timed out."), 5000);
  report("Connecting to the synthetic echo target…");
  socket.addEventListener("open", () => {
    if (session !== current) return;
    clearTimeout(current.timer);
    if (socket.protocol !== "binary") {
      close("Probe stopped: binary subprotocol was not negotiated.");
      return;
    }
    report("Connected. Sending one 1 KiB synthetic sample per second.");
    sendSample(current);
  });
  socket.addEventListener("message", (event) => {
    if (session !== current) return;
    try {
      if (!(event.data instanceof ArrayBuffer)) throw new Error("Binary data required");
      current.offset = verifyChunk(current.expected, current.offset, new Uint8Array(event.data));
    } catch {
      close("Probe stopped: unexpected or corrupted echo data.");
      return;
    }
    if (current.offset === current.expected.length) {
      clearTimeout(current.timer);
      const elapsed = performance.now() - current.sentAt;
      current.expected = undefined;
      current.sequence += 1;
      if (current.sequence === 1 || current.sequence % 10 === 0) {
        report(`${current.sequence} samples verified. Latest echo round trip: ${elapsed.toFixed(1)} ms (not desktop latency).`);
      }
      current.next = setTimeout(() => sendSample(current), 1000);
    }
  });
  socket.addEventListener("close", () => {
    if (session === current) close("Disconnected. Reload to sign in if Access expired.");
  });
  socket.addEventListener("error", () => {
    if (session === current) close("Connection failed. Check Access and the private gateway.");
  });
});
disconnect.addEventListener("click", () => close("Probe stopped."));
window.addEventListener("pagehide", () => close("Probe stopped."));
