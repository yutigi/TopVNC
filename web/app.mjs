// The page: pairing, sign-in, the viewer, and input. Protocol bytes never
// pass through here; the session (session.mjs) runs in a Worker when the
// browser can transfer the canvas to one, otherwise on this page.
// Cloudflare relays only ciphertext but serves this code, so it is trusted
// not to substitute it (see docs/cloudflare-gateway.md).

import init, { fingerprint, newBrowserKey, parseInvite, publicKey } from "./build/pkg/topvnc_web.js";
import { forgetPairing, inviteFromHash, loadPairing, savePairing } from "./pairing-store.mjs";
import { TouchGestures } from "./gestures.mjs";
import { attachTextInput } from "./text-input.mjs";
import { View } from "./view.mjs";

const $ = (selector) => document.querySelector(selector);
const home = $("#home");
const identity = $("#identity");
const connectForm = $("#connect-form");
const connectButton = $("#connect");
const forgetButton = $("#forget");
const passwordForm = $("#password-form");
const passwordInput = $("#password");
const statusLine = $("#status");
const log = $("#log");
const viewer = $("#viewer");
const screen = $("#screen");
const canvas = $("#canvas");
const cursor = $("#cursor");
const statsLine = $("#stats");
const toolbar = $("#toolbar");
const functionKeys = $("#function-keys");
const menu = $("#menu");
const keyboard = $("#keyboard");

const BUTTON = { left: 1, middle: 2, right: 4, wheelUp: 8, wheelDown: 16, wheelLeft: 32, wheelRight: 64, back: 128, forward: 256 };
// KeyboardEvent.buttons bits, in order: left, right, middle, back, forward.
const DOM_BUTTONS = [BUTTON.left, BUTTON.right, BUTTON.middle, BUTTON.back, BUTTON.forward];
const MODIFIERS = { ctrl: 0xffe3, alt: 0xffe9, shift: 0xffe1, super: 0xffeb };
const CLOSE_HINTS = {
  "browser key is not enrolled": "This browser is not enrolled on the host. Pair it again: Pair a browser in TopVNC's WEB tab (or the gateway's --pair), then scan the code.",
  "no pairing invite is active": "The pairing invite expired or was already used. Open a new one with Pair a browser in TopVNC's WEB tab (or the gateway's --pair).",
  "pairing invite expired or already used": "The pairing invite expired or was already used. Open a new one with Pair a browser in TopVNC's WEB tab (or the gateway's --pair).",
  "E2E handshake failed": "The host rejected the encrypted handshake: the pinned host key or the pairing secret does not match.",
  "another browser session is active": "Another browser is connected. Disconnect it there, or wait about a minute for it to time out.",
  "RFB host offline": "The gateway is running, but TopVNC is not serving on the host. Start topvnc --serve.",
  "Session expired": "The session reached its time limit, or the Cloudflare Access sign-in expired. Reload and connect again.",
  "Replaced by a newer connection from this browser": "This session was replaced by a newer connection from this browser.",
  "Peer idle": "Disconnected after a minute without traffic, as when the phone sleeps.",
  "Slow WebSocket peer": "The connection was too slow, so the host closed it.",
  "Slow TCP peer": "TopVNC on the host stopped reading, so the gateway closed the connection.",
};

let storage;
try {
  storage = window.localStorage;
} catch {
  storage = undefined;
}

let pending; // A scanned invite and a new browser key, kept in memory until verified.
let state = "idle"; // idle | connecting | password | connected
let frame = { width: 0, height: 0 };
// The host serves more than this browser shows; a size that fits was asked for.
let oversized = false;
let relativePointer = false;
let mode = preference("mode", "trackpad");
let buttons = 0;
let pointerPosition = { x: 0, y: 0 };
// Mouse users see their own cursor; touch input aims a drawn one.
let touchInput = matchMedia("(pointer: coarse)").matches;
const latched = new Set();
const view = new View();
let lines = [];
let post;

function preference(name, fallback) {
  try {
    return storage?.getItem(`topvnc.viewer.${name}`) ?? fallback;
  } catch {
    return fallback;
  }
}

function remember(name, value) {
  try {
    storage?.setItem(`topvnc.viewer.${name}`, String(value));
  } catch {
    // Preferences are optional.
  }
}

// Radio groups drawn as the desktop app's rows of buttons.
function chosen(name) {
  return [...document.getElementsByName(name)].find((radio) => radio.checked)?.value;
}

function choose(name, value) {
  const radio = [...document.getElementsByName(name)].find((input) => input.value === String(value));
  if (radio) radio.checked = true;
}

function setQuality(value) {
  choose("quality", value);
  choose("menu-quality", value);
}

function report(message) {
  statusLine.textContent = message;
  lines.push(`${new Date().toLocaleTimeString()} ${message}`);
  lines = lines.slice(-40);
  log.textContent = lines.join("\n");
}

function pairing() {
  return pending ?? loadPairing(storage);
}

function describeIdentity() {
  const current = pairing();
  const busy = state !== "idle";
  connectButton.disabled = busy || !current;
  forgetButton.disabled = busy || !current || Boolean(pending);
  if (!current) {
    identity.textContent = "Not paired. On the host, choose Pair a browser in TopVNC's WEB tab (or run the gateway with --pair), then scan its QR code with the camera. On the Access route, sign in here first.";
    return;
  }
  try {
    const host = fingerprint(current.host);
    const browser = fingerprint(publicKey(current.browser));
    identity.textContent = pending
      ? `Pairing invite for host ${host}. Compare it with the host console, then connect to enroll this browser (${browser}).`
      : `Paired with host ${host}. This browser: ${browser}.`;
  } catch {
    identity.textContent = "The saved pairing is invalid. Forget it and pair again.";
    connectButton.disabled = true;
  }
}

// A #pair= link, from the camera or pasted into this tab. The fragment is
// removed from the address bar and this history entry at once.
function readInvite() {
  const invite = inviteFromHash(location.hash);
  if (location.hash) history.replaceState(null, "", location.pathname + location.search);
  if (!invite) return;
  let parsed;
  try {
    parsed = parseInvite(invite);
  } catch {
    report("The pairing link is invalid. Scan the host's QR code again.");
    return;
  }
  const saved = loadPairing(storage);
  if (saved && saved.host === parsed.host) {
    // A used or reopened link must not replace a working pairing.
    report("This browser is already paired with this host; the pairing link was ignored.");
  } else {
    pending = { host: parsed.host, secret: parsed.secret, browser: newBrowserKey() };
    report(saved
      ? "Pairing invite read for a different host key. Connecting replaces this browser's pairing."
      : "Pairing invite read. Compare the host fingerprint with the host console, then connect.");
  }
  parsed.free();
  describeIdentity();
}

// ---- Session messages -----------------------------------------------------

async function closeReason(message) {
  if (message.detail) report(`Detail: ${message.detail}`);
  if (Object.hasOwn(CLOSE_HINTS, message.reason)) return CLOSE_HINTS[message.reason];
  if (message.code >= 4000 && message.reason) return message.reason;
  if (message.reason) return `The gateway closed the connection: ${message.reason}.`;
  if (message.connected) return "The connection was lost. iOS closes connections of pages in the background.";
  // A failed upgrade looks the same for an expired sign-in and an offline
  // gateway; a page request tells them apart without following redirects.
  try {
    const response = await fetch("/", { redirect: "manual", cache: "no-store" });
    if (response.type === "opaqueredirect" || response.status === 302) {
      return "Your Cloudflare Access sign-in expired. Reload the page to sign in again.";
    }
  } catch {
    return "The network is unavailable.";
  }
  if (location.hostname.endsWith(".trycloudflare.com")) {
    return "The host is unavailable. A temporary Quick Tunnel address stops working when browser access stops; scan the host's new code.";
  }
  return "The private gateway is unavailable: the host, its tunnel connector, or the gateway is not running.";
}

async function onSessionMessage(message) {
  switch (message.type) {
    case "secure":
      report("Encrypted channel to the host established. Signing in to TopVNC…");
      break;
    case "verified":
      if (pending) {
        try {
          savePairing(storage, pending);
          report("This browser is enrolled. The pairing is saved in this browser only.");
        } catch {
          report("Enrolled, but this browser could not save the pairing. Pair again next time.");
        }
        pending = undefined;
        describeIdentity();
      }
      break;
    case "password":
      state = "password";
      passwordForm.hidden = false;
      passwordInput.value = "";
      passwordInput.focus();
      report("Enter the VNC password set on the host. It is sent only inside the encrypted channel and is not stored.");
      break;
    case "connected":
      state = "connected";
      passwordForm.hidden = true;
      passwordInput.value = "";
      showViewer(true);
      report(`Connected to ${message.name || "the host"}.`);
      break;
    case "resized":
      frame = { width: message.width, height: message.height };
      pointerPosition = { x: Math.floor(frame.width / 2), y: Math.floor(frame.height / 2) };
      layout();
      if (oversized && message.width > 1) {
        oversized = false;
        report(`The host now shares ${message.width}×${message.height}, which fits this browser.`);
      }
      break;
    case "oversize":
      // The session asks the host for a size that fits; a 1×1 placeholder
      // is shown until it arrives.
      oversized = true;
      report(`The host shares ${message.width}×${message.height}, more than this browser can show. Asking it to share a smaller size, or one display instead of all…`);
      break;
    case "pointerMode":
      relativePointer = message.relative;
      if (!relativePointer && document.pointerLockElement) document.exitPointerLock();
      updateCursor();
      break;
    case "capabilities":
      showCapabilities(message);
      break;
    case "stats": {
      const rtt = message.rtt >= 0 ? ` · RTT ${message.rtt.toFixed(0)} ms` : "";
      statsLine.textContent = `${message.fps.toFixed(0)} updates/s · ${message.presentedFps.toFixed(0)} drawn/s · ${message.mbps.toFixed(1)} Mbit/s${rtt}${message.continuous ? " · push" : ""}`;
      break;
    }
    case "closed": {
      const wasConnected = message.connected;
      oversized = false;
      resetInput();
      state = "idle";
      passwordForm.hidden = true;
      passwordInput.value = "";
      showViewer(false);
      // The next host may not offer these controls.
      showCapabilities({ serveScale: 0, displays: [] });
      describeIdentity();
      report(`${wasConnected ? "Disconnected" : "Connection failed"} (${message.code}). ${await closeReason(message)}`);
      connectButton.textContent = wasConnected ? "Reconnect" : "Connect";
      break;
    }
    default:
      break;
  }
}

function send(message) {
  post?.(message);
}

function input(message) {
  if (state === "connected") send({ type: "input", ...message });
}

// ---- Viewer layout ----------------------------------------------------------

function showViewer(visible) {
  viewer.hidden = !visible;
  home.hidden = visible;
  document.body.classList.toggle("viewing", visible);
  if (visible) layout();
  else {
    showMenu(false);
    showFunctionKeys(false);
    keyboard.blur();
  }
}

function showMenu(visible) {
  menu.hidden = !visible;
  $("#show-menu").setAttribute("aria-expanded", String(visible));
}

function showFunctionKeys(visible) {
  functionKeys.hidden = !visible;
  $("#show-function-keys").setAttribute("aria-expanded", String(visible));
}

function layout() {
  const box = screen.getBoundingClientRect();
  view.layout(box.width, box.height, frame.width, frame.height);
  Object.assign(canvas.style, view.style());
  updateCursor();
}

function updateCursor() {
  // A local cursor where touch input aims; mouse users see their own.
  const visible = state === "connected" && touchInput && mode === "trackpad" && !relativePointer && frame.width > 0;
  cursor.hidden = !visible;
  if (!visible) return;
  const at = view.fromFrame(pointerPosition.x, pointerPosition.y, frame.width, frame.height);
  cursor.style.transform = `translate(${at.x}px, ${at.y}px)`;
}

// Hosts that do not offer a control say so, as in the desktop app.
function showCapabilities({ serveScale, displays }) {
  for (const element of [$("#serve-scale"), $("#serve-scale-ends")]) element.hidden = !serveScale;
  $("#serve-scale-missing").hidden = Boolean(serveScale);
  showServeScale(serveScale);
  const display = $("#serve-display");
  display.hidden = !displays.length;
  $("#serve-display-missing").hidden = displays.length > 0;
  if (displays.length) {
    const [served, count] = displays;
    // Each display from 1, then all of them (0).
    const choices = [...Array(count).keys()].map((index) => index + 1).concat(0);
    display.replaceChildren(...choices.map((number) => displayChoice(number, served)));
  }
}

function displayChoice(number, served) {
  const choice = document.createElement("label");
  choice.className = "choice";
  const radio = Object.assign(document.createElement("input"), {
    type: "radio", name: "serve-display", value: String(number), checked: number === served,
  });
  radio.setAttribute("aria-label", number === 0 ? "All displays" : `Display ${number}`);
  const text = document.createElement("span");
  text.textContent = number === 0 ? "All" : String(number);
  choice.append(radio, text);
  return choice;
}

// The served percentage, or 0 when the host offers no control.
function showServeScale(percent) {
  const scale = $("#serve-scale");
  if (percent) scale.value = String(percent);
  scale.style.setProperty("--fill", `${((Number(scale.value) - 25) / 75) * 100}%`);
  $("#serve-scale-value").textContent = percent ? `${scale.value}% shared` : "Unavailable";
}

// ---- Pointer input ------------------------------------------------------------

function screenPoint(event) {
  const box = screen.getBoundingClientRect();
  return { x: event.clientX - box.left, y: event.clientY - box.top };
}

function sendPointer(motion) {
  if (relativePointer) input({ kind: "motion", buttons, dx: 0, dy: 0, buttonsChanged: !motion });
  else input({ kind: "pointer", buttons, x: pointerPosition.x, y: pointerPosition.y, motion });
}

function moveTo(point) {
  pointerPosition = view.toFrame(point.x, point.y, frame.width, frame.height);
  updateCursor();
  sendPointer(true);
}

function moveBy(dx, dy) {
  // Mild acceleration, like a laptop trackpad.
  const gain = view.framePerViewPixel(frame.width) * (1 + Math.min(2, Math.hypot(dx, dy) / 12));
  if (relativePointer) {
    input({ kind: "motion", buttons, dx: Math.round(dx * gain), dy: Math.round(dy * gain) });
    return;
  }
  pointerPosition = {
    x: Math.min(frame.width - 1, Math.max(0, pointerPosition.x + dx * gain)),
    y: Math.min(frame.height - 1, Math.max(0, pointerPosition.y + dy * gain)),
  };
  updateCursor();
  input({ kind: "pointer", buttons, x: Math.round(pointerPosition.x), y: Math.round(pointerPosition.y), motion: true });
}

function setButton(bit, down) {
  const next = down ? buttons | bit : buttons & ~bit;
  if (next === buttons) return;
  buttons = next;
  sendPointer(false);
}

function wheel(stepsX, stepsY) {
  const notches = [
    [Math.max(0, -stepsY), BUTTON.wheelUp],
    [Math.max(0, stepsY), BUTTON.wheelDown],
    [Math.max(0, -stepsX), BUTTON.wheelLeft],
    [Math.max(0, stepsX), BUTTON.wheelRight],
  ];
  for (const [count, bit] of notches) {
    for (let index = 0; index < Math.min(count, 20); index++) {
      setButton(bit, true);
      setButton(bit, false);
    }
  }
}

const gestures = new TouchGestures({
  move: (to) => (to.dx === undefined ? moveTo(to) : moveBy(to.dx, to.dy)),
  button: (name, down) => setButton(BUTTON[name], down),
  scroll: (dx, dy) => wheel(dx, dy),
  zoom: (factor, cx, cy) => {
    view.zoomAt(factor, cx, cy);
    layout();
  },
  pan: (dx, dy) => {
    view.pan(dx, dy);
    layout();
  },
}, mode);

screen.addEventListener("pointerdown", (event) => {
  if (state !== "connected") return;
  event.preventDefault();
  keyboard.blur();
  if (touchInput !== (event.pointerType === "touch")) {
    touchInput = event.pointerType === "touch";
    updateCursor();
  }
  if (event.pointerType === "touch") {
    const point = screenPoint(event);
    gestures.down(event.pointerId, point.x, point.y, event.timeStamp);
    return;
  }
  screen.setPointerCapture(event.pointerId);
  if (relativePointer && !document.pointerLockElement) {
    canvas.requestPointerLock?.();
    return;
  }
  if (!relativePointer) moveTo(screenPoint(event));
  domButtons(event.buttons);
});

screen.addEventListener("pointermove", (event) => {
  if (state !== "connected") return;
  if (event.pointerType === "touch") {
    const point = screenPoint(event);
    gestures.move(event.pointerId, point.x, point.y, event.timeStamp);
    return;
  }
  if (document.pointerLockElement === canvas) {
    input({ kind: "motion", buttons, dx: Math.round(event.movementX), dy: Math.round(event.movementY) });
    return;
  }
  if (!relativePointer) moveTo(screenPoint(event));
  domButtons(event.buttons);
});

for (const type of ["pointerup", "pointercancel"]) {
  screen.addEventListener(type, (event) => {
    if (state !== "connected") return;
    if (event.pointerType === "touch") {
      const point = screenPoint(event);
      if (type === "pointercancel") gestures.cancel();
      else gestures.up(event.pointerId, point.x, point.y, event.timeStamp);
      return;
    }
    domButtons(type === "pointercancel" ? 0 : event.buttons);
  });
}

function domButtons(mask) {
  DOM_BUTTONS.forEach((bit, index) => setButton(bit, Boolean(mask & (1 << index))));
}

let wheelX = 0;
let wheelY = 0;
screen.addEventListener("wheel", (event) => {
  if (state !== "connected") return;
  event.preventDefault();
  if (event.ctrlKey) {
    // Trackpad pinch on desktop browsers zooms the local view.
    view.zoomAt(Math.exp(-event.deltaY / 100), ...Object.values(screenPoint(event)));
    layout();
    return;
  }
  const scale = event.deltaMode === 1 ? 1 / 3 : event.deltaMode === 2 ? 1 : 1 / 100;
  wheelX += event.deltaX * scale;
  wheelY += event.deltaY * scale;
  const stepsX = Math.trunc(wheelX);
  const stepsY = Math.trunc(wheelY);
  wheelX -= stepsX;
  wheelY -= stepsY;
  wheel(stepsX, stepsY);
}, { passive: false });

screen.addEventListener("contextmenu", (event) => event.preventDefault());

// ---- Keyboard input -----------------------------------------------------------

function releaseLatched() {
  for (const name of latched) {
    input({ kind: "keysym", keysym: MODIFIERS[name], down: false });
    toolbar.querySelector(`[data-modifier="${name}"]`)?.classList.remove("latched");
  }
  latched.clear();
}

function tapKeysym(keysym) {
  input({ kind: "keysym", keysym, down: true });
  input({ kind: "keysym", keysym, down: false });
  releaseLatched();
}

const MODIFIER_CODES = /^(Shift|Control|Alt|Meta|OS)(Left|Right)$/;

window.addEventListener("keydown", (event) => {
  if (state !== "connected" || event.target === keyboard || event.target.closest?.("#menu")) return;
  // Hardware keyboards report physical keys; text input methods use the
  // on-screen keyboard field instead.
  if (event.isComposing || !event.code || event.code === "Unidentified") return;
  event.preventDefault();
  input({ kind: "code", code: event.code, down: true });
});

window.addEventListener("keyup", (event) => {
  if (state !== "connected" || event.target === keyboard || !event.code) return;
  event.preventDefault();
  input({ kind: "code", code: event.code, down: false });
  if (!MODIFIER_CODES.test(event.code)) releaseLatched();
});

attachTextInput(keyboard, {
  active: () => state === "connected",
  text: (text) => {
    input({ kind: "text", text });
    releaseLatched();
  },
  tap: tapKeysym,
});

keyboard.addEventListener("keydown", (event) => {
  if (state !== "connected") return;
  // Keys without text, from either keyboard.
  const keysym = { Escape: 0xff1b, Tab: 0xff09, ArrowLeft: 0xff51, ArrowUp: 0xff52, ArrowRight: 0xff53, ArrowDown: 0xff54 }[event.key];
  if (keysym) {
    event.preventDefault();
    tapKeysym(keysym);
  }
});

// ---- Toolbar and menu ----------------------------------------------------------

toolbar.addEventListener("click", (event) => {
  const button = event.target.closest("button");
  if (!button || state !== "connected") return;
  if (button.dataset.keysym) tapKeysym(Number(button.dataset.keysym));
  else if (button.dataset.modifier) {
    const name = button.dataset.modifier;
    const down = !latched.has(name);
    if (down) latched.add(name);
    else latched.delete(name);
    button.classList.toggle("latched", down);
    input({ kind: "keysym", keysym: MODIFIERS[name], down });
  } else if (button.id === "show-keyboard") {
    keyboard.focus();
  } else if (button.id === "show-function-keys") {
    showFunctionKeys(functionKeys.hidden);
    layout();
  } else if (button.id === "toggle-mode") {
    mode = mode === "trackpad" ? "direct" : "trackpad";
    remember("mode", mode);
    gestures.setMode(mode);
    button.textContent = mode === "trackpad" ? "Trackpad" : "Direct";
    updateCursor();
  } else if (button.id === "fit") {
    view.reset();
    layout();
  } else if (button.id === "show-menu") {
    showMenu(menu.hidden);
  }
});

functionKeys.addEventListener("click", (event) => {
  const button = event.target.closest("button");
  if (button?.dataset.keysym) tapKeysym(Number(button.dataset.keysym));
});

// Keep the toolbar from taking focus away from the on-screen keyboard.
for (const bar of [toolbar, functionKeys]) {
  bar.addEventListener("pointerdown", (event) => {
    const button = event.target.closest("button");
    if (button && button.id !== "show-keyboard") event.preventDefault();
  });
}

$("#disconnect").addEventListener("click", () => {
  input({ kind: "release" });
  send({ type: "close" });
});
$("#stats-toggle").addEventListener("change", (event) => {
  statsLine.hidden = !event.target.checked;
});
$("#menu-quality").addEventListener("change", (event) => {
  choose("quality", event.target.value);
  remember("quality", event.target.value);
  input({ kind: "quality", quality: Number(event.target.value) });
});
$("#serve-scale").addEventListener("input", (event) => showServeScale(Number(event.target.value)));
$("#serve-scale").addEventListener("change", (event) => input({ kind: "serveScale", percent: Number(event.target.value) }));
$("#serve-display").addEventListener("change", (event) => input({ kind: "serveDisplay", display: Number(event.target.value) }));
$("#close-menu").addEventListener("click", () => showMenu(false));

// ---- Connecting -----------------------------------------------------------------

let worker;
let transferred = false;

async function startSession() {
  if (!post) {
    if (typeof canvas.transferControlToOffscreen === "function" && typeof Worker === "function") {
      worker = new Worker(new URL("./session-worker.js", import.meta.url), { type: "module" });
      worker.onmessage = (event) => onSessionMessage(event.data);
      worker.onerror = () => onSessionMessage({ type: "closed", code: 4007, reason: "The viewer worker failed to start." });
      post = (message, transfer) => worker.postMessage(message, transfer ?? []);
    } else {
      const { createSession } = await import("./session.mjs");
      const handle = createSession((message) => onSessionMessage(message));
      post = (message) => handle(message);
    }
  }
  const keys = pairing();
  const message = {
    type: "connect",
    // Always this page's own origin; nothing chooses another destination.
    url: `${location.protocol === "https:" ? "wss" : "ws"}://${location.host}/ws`,
    browser: keys.browser,
    host: keys.host,
    secret: pending?.secret,
    quality: Number(chosen("quality")),
  };
  if (worker && !transferred) {
    message.canvas = canvas.transferControlToOffscreen();
    transferred = true;
    post(message, [message.canvas]);
  } else {
    if (!worker) message.canvas = canvas;
    post(message);
  }
}

connectForm.addEventListener("submit", (event) => {
  event.preventDefault();
  if (state !== "idle" || !pairing()) return;
  // Plain HTTP only for the loopback test harness (local_web example).
  const loopback = location.protocol === "http:" && ["127.0.0.1", "localhost", "[::1]"].includes(location.hostname);
  if (location.protocol !== "https:" && !loopback) {
    report("Use the HTTPS address the host shows.");
    return;
  }
  state = "connecting";
  describeIdentity();
  report(pending ? "Connecting to enroll this browser…" : "Connecting to the host…");
  startSession().catch(() => onSessionMessage({ type: "closed", code: 4007, reason: "The viewer failed to start." }));
});

passwordForm.addEventListener("submit", (event) => {
  event.preventDefault();
  if (state !== "password") return;
  const password = passwordInput.value;
  passwordInput.value = "";
  passwordForm.hidden = true;
  state = "connecting";
  send({ type: "password", password });
  report("Checking the password…");
});

forgetButton.addEventListener("click", () => {
  if (state !== "idle") return;
  forgetPairing(storage);
  pending = undefined;
  describeIdentity();
  report("Pairing removed from this browser. The host still lists its key until you remove it there.");
});

$("#quality").addEventListener("change", (event) => {
  remember("quality", event.target.value);
  choose("menu-quality", event.target.value);
});

// ---- Page lifecycle ---------------------------------------------------------------

function resetInput() {
  gestures.cancel();
  buttons = 0;
  for (const name of latched) toolbar.querySelector(`[data-modifier="${name}"]`)?.classList.remove("latched");
  latched.clear();
  if (document.pointerLockElement) document.exitPointerLock();
}

// Nothing stays held on the host when the page loses focus or hides.
function releaseEverything() {
  if (state === "connected") input({ kind: "release" });
  resetInput();
}

window.addEventListener("blur", releaseEverything);
document.addEventListener("visibilitychange", () => {
  if (document.hidden) releaseEverything();
  else if (state === "connected") {
    send({ type: "redraw" });
    layout();
  }
});
window.addEventListener("pagehide", () => {
  releaseEverything();
  send({ type: "close" });
});
window.addEventListener("hashchange", readInvite);
window.addEventListener("resize", layout);
window.visualViewport?.addEventListener("resize", () => {
  // The on-screen keyboard shrinks the visual viewport; keep the toolbar above it.
  const inset = Math.max(0, window.innerHeight - window.visualViewport.height - window.visualViewport.offsetTop);
  document.documentElement.style.setProperty("--keyboard-inset", `${inset}px`);
  layout();
});
new ResizeObserver(layout).observe(screen);

async function main() {
  await init({ module_or_path: "/topvnc_web_bg.wasm" });
  setQuality(preference("quality", "6"));
  $("#toggle-mode").textContent = mode === "trackpad" ? "Trackpad" : "Direct";
  readInvite();
  describeIdentity();
}

main().catch(() => report("The encryption module failed to load. Connections stay disabled."));
