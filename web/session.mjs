// The connection side of the viewer: the WebSocket to /ws, the WebAssembly
// session (Noise channel, RFB, decoding), and drawing to the canvas. It runs
// in a dedicated Worker with an OffscreenCanvas, so decoding never blocks
// touch input, or on the page itself where OffscreenCanvas is missing.
// Messages to and from the page are small plain objects.

import init, { Connection } from "./build/pkg/topvnc_web.js";

// Bits returned by Connection.receive (crates/topvnc-web/src/session.rs).
export const EVENT = {
  SECURE: 1 << 0,
  PASSWORD: 1 << 1,
  CONNECTED: 1 << 2,
  FRAME: 1 << 3,
  RESIZED: 1 << 4,
  BELL: 1 << 5,
  POINTER_MODE: 1 << 6,
  CAPABILITIES: 1 << 7,
  RTT: 1 << 8,
};

// Pointer motion is dropped (the latest position is kept) while this much is
// waiting to be sent; buttons and keys are never dropped.
const MOTION_BACKLOG_BYTES = 64 * 1024;
// A link this far behind is not usable for interactive control.
const MAX_BACKLOG_BYTES = 4 * 1024 * 1024;
const HANDSHAKE_TIMEOUT_MS = 15_000;
// Statistics, including a round-trip probe, once a second.
const STATS_INTERVAL_MS = 1_000;

const later = (callback) =>
  typeof requestAnimationFrame === "function" ? requestAnimationFrame(callback) : setTimeout(callback, 16);

export function createSession(post) {
  let wasm;
  let session;
  // The OffscreenCanvas arrives once and is reused by later connections.
  let canvas;

  async function ready() {
    wasm ??= await init({ module_or_path: "/topvnc_web_bg.wasm" });
  }

  function close(code, reason, detail) {
    if (!session) return;
    const current = session;
    session = undefined;
    clearInterval(current.timers);
    clearTimeout(current.deadline);
    try {
      current.connection.releaseAll();
      sendAll(current);
    } catch {
      // The connection already failed; nothing more is sent.
    }
    current.socket.onmessage = current.socket.onclose = current.socket.onerror = null;
    try {
      current.socket.close(1000, "Viewer closed");
    } catch {
      // Already closed.
    }
    current.connection.free();
    post({ type: "closed", code, reason, detail, connected: current.connected, secure: current.secure });
  }

  function sendAll(current) {
    const socket = current.socket;
    // Keep the first Noise message queued while the upgrade is pending.
    // Statistics and input can call this before the socket opens.
    if (socket.readyState !== WebSocket.OPEN) return;
    for (let message; (message = current.connection.takeOutgoing()); ) {
      socket.send(message);
    }
    if (socket.bufferedAmount > MAX_BACKLOG_BYTES) {
      close(4001, "The network is too slow to send input in time.");
    }
  }

  // An ImageData over the framebuffer in WebAssembly memory, without a
  // copy. Memory growth or a resize invalidates the view, so recheck it.
  function image(current) {
    const width = current.connection.width();
    const height = current.connection.height();
    if (!width || !height) return undefined;
    const memory = wasm.memory.buffer;
    const pointer = current.connection.framePointer();
    if (!current.image || current.image.width !== width || current.image.height !== height
      || current.imageBuffer !== memory || current.imagePointer !== pointer) {
      const pixels = new Uint8ClampedArray(memory, pointer, current.connection.frameLength());
      current.image = new ImageData(pixels, width, height);
      current.imageBuffer = memory;
      current.imagePointer = pointer;
    }
    return current.image;
  }

  // Draw only the regions changed since the last frame drawn.
  function render(current) {
    current.renderPending = false;
    if (session !== current || !current.context) return;
    const frame = image(current);
    if (!frame) return;
    const damage = current.connection.takeDamage();
    for (let index = 0; index + 3 < damage.length; index += 4) {
      current.context.putImageData(frame, 0, 0, damage[index], damage[index + 1], damage[index + 2], damage[index + 3]);
    }
    current.presented += damage.length ? 1 : 0;
  }

  function handle(current, events) {
    if (events & EVENT.SECURE) {
      current.secure = true;
      post({ type: "secure" });
    }
    if (events & EVENT.PASSWORD) {
      clearTimeout(current.deadline);
      post({ type: "password" });
    }
    if (events & EVENT.CONNECTED) {
      current.connected = true;
      clearTimeout(current.deadline);
      post({ type: "connected", name: current.connection.serverName() });
    }
    if (events & EVENT.RESIZED) {
      const width = current.connection.width();
      const height = current.connection.height();
      if (current.canvas) {
        current.canvas.width = width;
        current.canvas.height = height;
      }
      current.image = undefined;
      post({ type: "resized", width, height });
    }
    if (events & EVENT.POINTER_MODE) post({ type: "pointerMode", relative: current.connection.relativePointer() });
    if (events & EVENT.CAPABILITIES) {
      post({ type: "capabilities", serveScale: current.connection.serveScale(), displays: [...current.connection.servedDisplays()] });
    }
    if (events & EVENT.BELL) post({ type: "bell" });
    if (events & EVENT.FRAME && !current.renderPending) {
      current.renderPending = true;
      later(() => render(current));
    }
  }

  function stats(current) {
    const now = performance.now();
    const seconds = Math.max(0.001, (now - current.statsAt) / 1000);
    const bytes = current.connection.receivedBytes();
    const frames = current.connection.frames();
    post({
      type: "stats",
      fps: (frames - current.statsFrames) / seconds,
      presentedFps: current.presented / seconds,
      mbps: ((bytes - current.statsBytes) * 8) / seconds / 1e6,
      rtt: current.connection.rttMs(),
      continuous: current.connection.continuousUpdates(),
      backlog: current.socket.bufferedAmount,
    });
    current.statsAt = now;
    current.statsBytes = bytes;
    current.statsFrames = frames;
    current.presented = 0;
    current.connection.ping(now);
    sendAll(current);
  }

  async function connect({ url, browser, host, secret, quality, canvas: transferred }) {
    if (session) close(4000, "Replaced by a new connection.");
    canvas = transferred ?? canvas;
    await ready();
    const connection = new Connection(browser, host, secret ?? undefined, quality);
    const socket = new WebSocket(url, ["binary"]);
    socket.binaryType = "arraybuffer";
    const current = {
      connection,
      socket,
      canvas,
      context: canvas?.getContext("2d", { alpha: false }),
      secure: false,
      verified: false,
      connected: false,
      renderPending: false,
      presented: 0,
      statsAt: performance.now(),
      statsBytes: 0,
      statsFrames: 0,
    };
    session = current;
    current.deadline = setTimeout(() => close(4002, "The host did not answer in time."), HANDSHAKE_TIMEOUT_MS);
    socket.onopen = () => {
      if (socket.protocol !== "binary") return close(4003, "The binary WebSocket subprotocol was not negotiated.");
      sendAll(current);
    };
    socket.onmessage = (event) => {
      if (session !== current) return;
      if (!(event.data instanceof ArrayBuffer)) return close(4004, "The gateway sent text instead of binary data.");
      let events;
      try {
        events = connection.receive(new Uint8Array(event.data), performance.now());
      } catch (error) {
        const detail = String(error);
        if (!current.secure) {
          return close(4005, "The host identity could not be verified. No unencrypted connection was attempted.", detail);
        }
        if (/None security|no supported security type/.test(detail)) {
          return close(4005, "TopVNC on the host offers no VNC password. The browser client never connects without one.", detail);
        }
        return close(4005, `TopVNC on the host refused the session: ${detail}.`);
      }
      // The first message after the handshake decrypted: the host accepted
      // this browser, so an enrollment can be kept.
      if (current.secure && !current.verified && !(events & EVENT.SECURE)) {
        current.verified = true;
        post({ type: "verified" });
      }
      handle(current, events);
      sendAll(current);
    };
    socket.onclose = (event) => close(event.code, event.reason);
    socket.onerror = () => close(1006, "");
    current.timers = setInterval(() => stats(current), STATS_INTERVAL_MS);
  }

  // Input from the page. Motion is coalesced under backpressure.
  function input(current, message) {
    const connection = current.connection;
    switch (message.kind) {
      case "pointer":
        if (message.motion && current.socket.bufferedAmount > MOTION_BACKLOG_BYTES) {
          current.pendingPointer = message;
          return;
        }
        current.pendingPointer = undefined;
        connection.pointer(message.buttons, message.x, message.y);
        break;
      case "motion":
        if (current.socket.bufferedAmount > MOTION_BACKLOG_BYTES && !message.buttonsChanged) return;
        connection.pointerMotion(message.buttons, message.dx, message.dy);
        break;
      case "code":
        connection.keyCode(message.code, message.down);
        break;
      case "keysym":
        connection.keySym(message.keysym, message.down);
        break;
      case "text":
        connection.typeText(message.text);
        break;
      case "release":
        connection.releaseAll();
        break;
      case "quality":
        connection.setQuality(message.quality);
        break;
      case "serveScale":
        connection.requestServeScale(message.percent);
        break;
      case "serveDisplay":
        connection.requestServeDisplay(message.display);
        break;
      default:
        return;
    }
    sendAll(current);
  }

  function flushPending() {
    const current = session;
    if (current?.pendingPointer && current.socket.bufferedAmount <= MOTION_BACKLOG_BYTES) {
      const pending = current.pendingPointer;
      current.pendingPointer = undefined;
      current.connection.pointer(pending.buttons, pending.x, pending.y);
      sendAll(current);
    }
  }

  return async function onMessage(message) {
    try {
      if (message.type === "connect") return await connect(message);
      if (message.type === "close") return close(4000, "Disconnected.");
      const current = session;
      if (!current) return;
      if (message.type === "password") {
        const events = current.connection.password(message.password, performance.now());
        current.deadline = setTimeout(() => close(4002, "The host did not answer in time."), HANDSHAKE_TIMEOUT_MS);
        handle(current, events);
        sendAll(current);
      } else if (message.type === "input") {
        input(current, message);
        flushPending();
      } else if (message.type === "redraw") {
        // The page was hidden and may have lost the canvas contents.
        const frame = image(current);
        current.connection.takeDamage();
        if (current.context && frame) current.context.putImageData(frame, 0, 0);
      }
    } catch (error) {
      close(4006, "The viewer failed.", String(error));
    }
  };
}
