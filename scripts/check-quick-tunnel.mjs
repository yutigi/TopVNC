// Opt-in live validation, never part of automatic unit tests. Requires Node 22+,
// built native gateway/web assets, cloudflared, and Chromium. No third-party
// Node packages. Email/PIN entry is exclusively an interactive owner action.
import { spawn } from "node:child_process";
import { createHash, randomBytes } from "node:crypto";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { request } from "node:https";
import { join, resolve, sep } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { startRfb } from "./lib/quick-tunnel-rfb.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const exe = process.platform === "win32" ? ".exe" : "";
class CheckFailure extends Error {}
const check = (ok, label) => { if (!ok) throw new CheckFailure(label); };
const hash = value => createHash("sha256").update(value).digest("hex");

export function tunnelArgs(config, address, email) {
  return ["tunnel", "--no-autoupdate", "--config", config, "--url", `http://${address}`,
    ...(email ? ["--allowed-mail", email] : [])];
}

export function isolatedEnvironment(environment) {
  return Object.fromEntries(Object.entries(environment).filter(([key]) =>
    !key.toUpperCase().startsWith("TUNNEL_") && key !== "TOPVNC_TEST_EMAIL"));
}

export function isLoginResponse(response) {
  return response.status === 401 || response.status === 403 ||
    (response.status >= 300 && response.status < 400 && Boolean(response.location)) ||
    (response.status === 200 && /text\/html/i.test(response.contentType) &&
      /email|one.time|sign.in/i.test(response.body.toString()) &&
      !response.body.includes(Buffer.from('id="connect-form"')));
}

export function assessGate(page, asset, ws, expectedAssetHash) {
  const statuses = { page: page.status, asset: asset.status, websocket: ws.status,
    websocketHasLoginLocation: Boolean(ws.location) };
  let failure = null;
  if (!isLoginResponse(page)) failure = "Unauthenticated page must require login";
  else if (hash(asset.body) === expectedAssetHash || !isLoginResponse(asset)) failure = "Unauthenticated assets must require login";
  else if (ws.status === 101 || !isLoginResponse(ws)) failure = "Unauthenticated WebSocket must require login";
  return { statuses, failure };
}

// cloudflared 2026.10.3 quicktunnelauth/session.go. Other cookies (including
// temporary login-state cookies) are not evidence of the auth session expiry.
export function authSessionMetadata(cookies, now = Date.now() / 1000) {
  const session = cookies.filter(cookie => cookie.name === "__Host-cloudflared-qt-auth-session");
  const metadata = { authCookieCount: session.length, browserExpiryAt: null,
    signedExpiryClaimAt: null, remainingSeconds: null };
  if (session.length !== 1) return metadata;
  const cookie = session[0];
  if (Number.isFinite(cookie.expires) && cookie.expires > 0) metadata.browserExpiryAt = cookie.expires;
  // Read only the expiry, never persist the nonce, signature, or cookie value.
  // This is inspection, not independent verification of the HMAC signature.
  try {
    if (cookie.value.length <= 4096) {
      const parts = cookie.value.split(".");
      if (parts.length === 2 && /^[A-Za-z0-9_-]+$/.test(parts[0])) {
        const payload = JSON.parse(Buffer.from(parts[0], "base64url").toString("utf8"));
        if (Number.isSafeInteger(payload.exp) && payload.exp > 0) metadata.signedExpiryClaimAt = payload.exp;
      }
    }
  } catch { /* Unknown cookie format stays unverified. */ }
  const expiry = metadata.signedExpiryClaimAt ?? metadata.browserExpiryAt;
  if (expiry !== null) metadata.remainingSeconds = Math.ceil(expiry - now);
  return metadata;
}

async function pollUntil(fn, label, ms = 30_000, signal) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    check(!signal?.aborted, "Live checks interrupted; cleaning up owned processes");
    const result = await fn();
    if (result) return result;
    await delay(150);
  }
  throw new CheckFailure(`Timed out: ${label}`);
}

function probe(origin, path, upgrade = false, cookie) {
  return new Promise((resolveProbe, reject) => {
    const headers = upgrade ? { Connection: "Upgrade", Upgrade: "websocket", Origin: origin,
      "Sec-WebSocket-Version": "13", "Sec-WebSocket-Key": randomBytes(16).toString("base64"),
      "Sec-WebSocket-Protocol": "binary" } : {};
    headers["Cache-Control"] = "no-cache, no-store";
    headers.Pragma = "no-cache";
    if (cookie) headers.Cookie = cookie;
    const req = request(`${origin}${path}`, { headers, timeout: 8000, agent: false }, response => {
      const chunks = []; let size = 0;
      response.on("data", chunk => {
        size += chunk.length;
        if (size > 2_097_152) req.destroy(new CheckFailure("Probe response too large"));
        else chunks.push(chunk);
      });
      response.on("end", () => resolveProbe({ status: response.statusCode,
        location: response.headers.location, contentType: response.headers["content-type"] ?? "",
        body: Buffer.concat(chunks) }));
    });
    req.on("upgrade", (response, socket) => {
      socket.destroy();
      resolveProbe({ status: response.statusCode, body: Buffer.alloc(0), contentType: "" });
    });
    req.on("timeout", () => req.destroy(new CheckFailure("Probe timeout")));
    req.on("error", () => reject(new CheckFailure("HTTPS probe failed")));
    req.end();
  });
}

async function attach(endpoint) {
  const ws = new WebSocket(endpoint);
  await new Promise((resolveSocket, reject) => {
    ws.onopen = resolveSocket; ws.onerror = () => reject(new CheckFailure("Browser connection failed"));
  });
  let next = 0;
  const pending = new Map();
  ws.onmessage = event => {
    const message = JSON.parse(event.data);
    if (!message.id) return;
    const item = pending.get(message.id);
    if (!item) return;
    clearTimeout(item.timer); pending.delete(message.id);
    if (message.error) item.reject(new CheckFailure(`Browser command failed: ${item.method}`));
    else item.resolve(message.result);
  };
  ws.onclose = () => {
    for (const item of pending.values()) {
      clearTimeout(item.timer); item.reject(new CheckFailure("Browser closed"));
    }
    pending.clear();
  };
  return { ws, call(method, params = {}) {
    return new Promise((resolveCommand, reject) => {
      const id = ++next;
      const timer = setTimeout(() => { pending.delete(id); reject(new CheckFailure(`Browser timeout: ${method}`)); }, 15_000);
      pending.set(id, { resolve: resolveCommand, reject, timer, method });
      ws.send(JSON.stringify({ id, method, params }));
    });
  } };
}

export async function run(options = {}) {
  const email = options.email ?? process.env.TOPVNC_TEST_EMAIL;
  check(!email || (email.length <= 254 && /^[^\s,@*]+@[^\s,@*]+\.[^\s,@*]+$/.test(email)), "Supply one explicit allowed email address");
  const chrome = options.chrome ?? process.env.TOPVNC_TEST_CHROME;
  check(chrome && existsSync(chrome), "Set TOPVNC_TEST_CHROME to an installed Chromium executable");
  const cloudflared = options.cloudflared ?? process.env.CLOUDFLARED ?? "cloudflared";
  const expiryWaitSeconds = Number(options.expiryWaitSeconds ?? process.env.TOPVNC_TEST_EXPIRY_WAIT_SECONDS ?? 0);
  check(Number.isInteger(expiryWaitSeconds) && expiryWaitSeconds >= 0 && expiryWaitSeconds <= 10_800,
    "Expiry wait budget must be 0-10800 seconds");
  const gateway = join(root, "target", "debug", `topvnc-web-gateway${exe}`);
  const dist = join(root, "web", "dist");
  check(existsSync(gateway) && existsSync(join(dist, "app.js")), "Build the worktree gateway and web assets first");
  const output = resolve(options.output ?? join(root, "target", "quick-tunnel-checks", `${Date.now()}`));
  const allowedRoot = resolve(root, "target", "quick-tunnel-checks");
  check(output.startsWith(allowedRoot + sep), "Output must be inside target/quick-tunnel-checks");
  mkdirSync(output, { recursive: true });
  const scratch = mkdtempSync(join(output, "scratch-"));
  const report = { mode: email ? "email-gated" : "pairing-only", started: new Date().toISOString(),
    runs: [], naturalLoginExpiry: "not tested", physicalIPhone: "not tested", realDesktopWebTab: "not tested" };
  let phase = "setup", browser, cdp, rfb;
  const cancellation = new AbortController();
  const interrupt = () => cancellation.abort();
  process.once("SIGINT", interrupt); process.once("SIGTERM", interrupt);
  const until = (fn, label, ms) => pollUntil(fn, label, ms, cancellation.signal);
  const children = new Set();
  // Logs are parsed in memory only. Never include raw child output, browser
  // exceptions, command arguments, or credentials in errors or reports.
  const save = () => writeFileSync(join(output, "report.json"), JSON.stringify(report, null, 2));
  const progress = label => { phase = label; console.log(label); };
  function child(program, args, label, env = isolatedEnvironment(process.env)) {
    const p = spawn(program, args, { windowsHide: true, stdio: ["pipe", "pipe", "pipe"], env });
    p.startFailed = false;
    p.on("error", () => { p.startFailed = true; });
    children.add(p); p.on("exit", () => children.delete(p));
    p.label = label;
    // Drain both streams even when only one is parsed, avoiding backpressure.
    p.stdout.on("data", () => {}); p.stderr.on("data", () => {});
    p.stdin.on("error", () => {});
    return p;
  }
  async function stop(p, graceful = false) {
    if (!p || p.startFailed || p.exitCode !== null || p.signalCode !== null) { children.delete(p); return; }
    if (graceful) p.stdin.end(); else p.kill();
    try { await pollUntil(() => p.exitCode !== null || p.signalCode !== null, "owned child exit", 5000); }
    catch { p.kill(); await pollUntil(() => p.exitCode !== null || p.signalCode !== null, "owned child termination", 5000); }
  }
  async function evaluate(expression) {
    const value = await cdp.call("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    check(!value.exceptionDetails, "Browser evaluation failed");
    return value.result.value;
  }
  async function navigate(url) {
    await cdp.call("Page.navigate", { url });
  }
  const appReady = () => evaluate(`document.readyState === 'complete' && !!document.querySelector('#connect') && !document.querySelector('#identity').textContent.includes('Loading')`);
  const connected = () => evaluate(`!!document.querySelector('#viewer') && !document.querySelector('#viewer').hidden && document.querySelector('#canvas').width === ${320}`);
  async function connect(password) {
    await evaluate(`document.querySelector('#connect').click()`);
    await until(() => evaluate(`!document.querySelector('#password-form').hidden`), "mandatory VNC password prompt");
    await evaluate(`document.querySelector('#password').value=${JSON.stringify(password)}; document.querySelector('#password-form').requestSubmit()`);
    await until(connected, "encrypted authenticated viewer");
    check(await evaluate(`document.querySelector('#password').value === '' && location.hash === ''`), "Transient credentials cleared");
  }
  async function disconnect() {
    await evaluate(`document.querySelector('#disconnect').click()`);
    await until(async () => !await connected(), "browser disconnect");
  }
  async function gated(origin, cookie, observe = () => {}) {
    const page = await probe(origin, "/", false, cookie);
    const asset = await probe(origin, "/app.js", false, cookie);
    const ws = await probe(origin, "/ws", true, cookie);
    const assessment = assessGate(page, asset, ws, hash(readFileSync(join(dist, "app.js"))));
    observe(assessment.statuses);
    check(!assessment.failure, assessment.failure);
    return assessment.statuses;
  }
  async function startRoute() {
    const g = child(gateway, ["--supervised-quick-tunnel", "127.0.0.1:0", rfb.address, dist], "gateway");
    let address, fingerprint, ready = false, invite;
    let partial = "";
    g.stdout.on("data", chunk => {
      partial += chunk.toString();
      if (partial.length >= 65_536) { g.protocolFailed = true; partial = ""; return; }
      let at;
      while ((at = partial.indexOf("\n")) >= 0) {
        const line = partial.slice(0, at).trim(); partial = partial.slice(at + 1);
        if (line.startsWith("bound ")) address = line.slice(6);
        if (line.startsWith("fingerprint ")) fingerprint = line.slice(12);
        if (line.startsWith("ready ")) ready = true;
        if (line.startsWith("pair ")) invite = line.split(" ")[2];
      }
    });
    await until(() => {
      check(!g.startFailed && !g.protocolFailed && g.exitCode === null, "Gateway startup failed");
      return address;
    }, "gateway bound before publishing");
    check(/^127\.0\.0\.1:\d+$/.test(address), "Gateway must bind loopback");
    const config = join(scratch, `config-${randomBytes(4).toString("hex")}.json`);
    writeFileSync(config, JSON.stringify({ ingress: [{ service: `http://${address}` }] }));
    const t = child(cloudflared, tunnelArgs(config, address, email), "connector");
    let origin, registered = false, flagRejected = false, logs = "";
    const parse = chunk => {
      logs = (logs + chunk.toString()).slice(-65_536);
      origin ??= logs.match(/https:\/\/[a-z0-9]+(?:[a-z0-9-]*[a-z0-9])?\.trycloudflare\.com(?=[\s|]|$)/)?.[0];
      registered ||= /Registered tunnel connection connIndex=\d+/.test(logs);
      flagRejected ||= /flag provided but not defined|Incorrect Usage|unknown flag/i.test(logs);
    };
    t.stdout.on("data", parse); t.stderr.on("data", parse);
    await until(() => {
      check(!flagRejected && !t.startFailed && t.exitCode === null, "Restricted connector startup failed; no fallback attempted");
      return origin && registered;
    }, "tunnel registration", 90_000);
    g.stdin.write(`origin ${origin}\n`);
    await until(() => ready && fingerprint, "gateway ready on learned origin");
    return { g, t, origin, fingerprint, async invite() {
      invite = undefined; g.stdin.write("pair\n");
      return until(() => invite, "fresh pairing invite");
    }, async stop() { await stop(t); await stop(g, true); } };
  }
  try {
    const password = randomBytes(4).toString("hex");
    rfb = await startRfb(password);
    progress("Starting a disposable private browser");
    const profile = join(scratch, "browser-profile");
    browser = child(chrome, [...(email ? [] : ["--headless"]), "--incognito", "--no-first-run",
      "--no-default-browser-check", "--disable-sync", "--disable-extensions", "--remote-debugging-address=127.0.0.1",
      "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], "browser");
    let endpoint;
    browser.stderr.on("data", data => { endpoint ??= data.toString().match(/DevTools listening on (ws:\/\/\S+)/)?.[1]; });
    await until(() => endpoint, "private browser debug endpoint");
    progress("Attaching to the disposable browser over loopback");
    const target = await until(async () => {
      const targets = await (await fetch(`http://127.0.0.1:${new URL(endpoint).port}/json/list`)).json();
      return targets.find(target => target.type === "page");
    }, "private browser page target");
    cdp = await attach(target.webSocketDebuggerUrl);
    await cdp.call("Page.enable"); await cdp.call("Runtime.enable"); await cdp.call("Network.enable");
    let previous;
    for (let run = 1; run <= (email ? 1 : 2); run++) {
      progress(`Run ${run}: binding gateway, then registering ${email ? "email-restricted" : "pairing-only"} tunnel`);
      const route = await startRoute();
      const result = { run }; report.runs.push(result); save();
      if (email) {
        progress("Checking page, asset, and WebSocket rejection before login");
        result.beforeLogin = await until(async () => {
          try { return await gated(route.origin); } catch { return false; }
        }, "live email gate", 90_000);
      } else {
        await until(async () => { try { return (await probe(route.origin, "/")).status === 200; } catch { return false; } }, "public page", 90_000);
      }
      await navigate(route.origin);
      if (email) {
        progress("OWNER ACTION: sign in with email and PIN in the disposable Chromium window; do not send the PIN here");
        report.awaitingOwnerLogin = true; save();
      }
      await until(appReady, "interactive owner login and app load", email ? 900_000 : 30_000);
      report.awaitingOwnerLogin = false;
      if (email) {
        // Inspect expiry metadata only. Values never leave this disposable process.
        const { cookies } = await cdp.call("Network.getCookies", { urls: [route.origin] });
        result.authSession = authSessionMetadata(cookies);
      }
      if (previous) {
        const previousConnections = rfb.audit.connections;
        check(route.origin !== previous.origin && route.fingerprint !== previous.fingerprint, "Restart must create a fresh URL and host identity");
        check(await evaluate(`localStorage.getItem('topvnc.e2e.pairing.v1') === null`), "New origin must start unenrolled");
        await evaluate(`localStorage.setItem('topvnc.e2e.pairing.v1',${JSON.stringify(previous.pairing)})`);
        await navigate(route.origin); await until(appReady, "reloaded previous browser key");
        await evaluate(`document.querySelector('#connect').click()`);
        await until(() => evaluate(`/Connection failed/.test(document.querySelector('#status').textContent)`), "old pairing rejection");
        check(!await connected(), "Previous pairing must not display the new desktop");
        check(rfb.audit.connections === previousConnections, "Previous pairing must not open a desktop TCP connection");
        await evaluate("localStorage.clear()");
        result.freshUrlAndKey = true; result.oldPairingRejected = true;
      }
      progress(`Run ${run}: pairing, password, encrypted frames, and safe synthetic input`);
      await navigate(await route.invite()); await until(appReady, "invite page");
      check(await evaluate(`document.querySelector('#identity').textContent.includes(${JSON.stringify(route.fingerprint)})`), "Browser fingerprint must match the gateway");
      const firstFrame = rfb.audit.frames;
      await connect(password);
      // Capture only after the synthetic viewer replaces the home page;
      // never a pairing QR, login screen, real desktop, or saved screenshot.
      const pixelDigest = async () => {
        check(await evaluate(`document.querySelector('#home').hidden && !document.querySelector('#viewer').hidden`), "Only capture the synthetic viewer");
        return hash((await cdp.call("Page.captureScreenshot", { format: "png" })).data);
      };
      await until(() => rfb.audit.frames > firstFrame, "first framebuffer update");
      const first = await pixelDigest();
      const capturedFrame = rfb.audit.frames;
      await until(() => rfb.audit.frames > capturedFrame, "next framebuffer update");
      await until(async () => first !== await pixelDigest(), "changing rendered pixels");
      // The real session's socket lives in a Worker, outside the page's CDP
      // Network events. Also verify the HTTP upgrade directly with only this
      // disposable browser's origin-scoped cookies; never save their values.
      const { cookies: upgradeCookies } = await cdp.call("Network.getCookies", { urls: [route.origin] });
      if (email) result.upgradeAuthSession = authSessionMetadata(upgradeCookies);
      const upgrade = await probe(route.origin, "/ws", true,
        upgradeCookies.map(cookie => `${cookie.name}=${cookie.value}`).join("; "));
      check(upgrade.status === 101 && rfb.audit.authenticated > 0, "Authenticated WebSocket and VNC session required");
      result.authenticatedUpgrade = upgrade.status;
      result.encryptedFrames = true;
      // Actual browser key/pointer events target only the harmless synthetic server.
      const point = await evaluate(`(() => { const r=document.querySelector('#canvas').getBoundingClientRect(); return {x:r.x+r.width/2,y:r.y+r.height/2}; })()`);
      await cdp.call("Input.dispatchMouseEvent", { type: "mousePressed", button: "left", buttons: 1, clickCount: 1, ...point });
      await cdp.call("Input.dispatchKeyEvent", { type: "keyDown", key: "a", code: "KeyA", windowsVirtualKeyCode: 65 });
      await until(() => rfb.audit.buttons === 1 && rfb.audit.heldKeys.size > 0, "held synthetic input reached server");
      await evaluate("window.dispatchEvent(new Event('blur'))");
      await until(() => rfb.audit.buttons === 0 && rfb.audit.heldKeys.size === 0 && rfb.audit.keyUp > 0, "blur released held input");
      result.blurReleasedInput = true;
      previous = { origin: route.origin, fingerprint: route.fingerprint,
        pairing: await evaluate("localStorage.getItem('topvnc.e2e.pairing.v1')") };
      await disconnect(); await connect(password);
      result.enrolledReconnect = true;
      if (!email && run === 1) {
        // This exercises Chromium's page lifecycle, not physical iPhone Safari.
        result.chromiumFrozenPauses = [];
        for (const seconds of [2, 65]) {
          progress(`Run ${run}: freezing the test page for ${seconds} seconds, then resuming`);
          const started = Date.now();
          await cdp.call("Page.setWebLifecycleState", { state: "frozen" });
          while (Date.now() - started < seconds * 1000) {
            await delay(Math.max(1, Math.min(30_000, seconds * 1000 - (Date.now() - started))), undefined, { signal: cancellation.signal });
            console.log(`Chromium pause: ${Math.round((Date.now() - started) / 1000)} seconds`);
          }
          await cdp.call("Page.setWebLifecycleState", { state: "active" });
          await delay(1500); // Let a queued socket-close event reach the app.
          const remainedConnected = await connected();
          if (!remainedConnected) await connect(password);
          const resumed = rfb.audit.frames;
          await until(() => rfb.audit.frames > resumed, "frames after Chromium resume");
          result.chromiumFrozenPauses.push({ seconds, remainedConnected, recoveredWithoutInvite: true });
        }
      }
      if (email) {
        const recordRejection = (name, passed, reason) => {
          result[`${name}Passed`] = passed;
          if (!passed) {
            report.failedPhase ??= phase;
            report.failedCheck ??= reason;
            report.failedChecks ??= [];
            report.failedChecks.push({ check: name, reason });
            report.securityChecksFailed = true;
            process.exitCode = 1;
          }
          save();
          return passed;
        };
        const rejectionCheck = async (name, cookie) => {
          try {
            await gated(route.origin, cookie,
              statuses => { result[name] = statuses; save(); });
            return recordRejection(name, true);
          } catch (error) {
            if (!(error instanceof CheckFailure)) throw error;
            return recordRejection(name, false, error.message);
          }
        };
        progress("Checking a fresh client without login after the authenticated session");
        await rejectionCheck("freshClientAfterLogin");
        const expiry = result.upgradeAuthSession.signedExpiryClaimAt ?? result.upgradeAuthSession.browserExpiryAt;
        if (expiryWaitSeconds && expiry && expiry - Date.now() / 1000 <= expiryWaitSeconds) {
          progress("Waiting for recorded login-cookie expiry; retaining original cookies only in memory");
          while (Date.now() / 1000 < expiry + 2) {
            await delay(Math.min(30_000, Math.max(1, (expiry + 2) * 1000 - Date.now())), undefined, { signal: cancellation.signal });
            console.log("Waiting for natural cookie expiry");
          }
          result.activeSessionAfterCookieExpiry = await connected();
          const rejected = await rejectionCheck("replayedCookiesAfterExpiry",
            upgradeCookies.map(cookie => `${cookie.name}=${cookie.value}`).join("; "));
          report.naturalLoginExpiry = rejected
            ? "original cookies rejected after recorded auth-session expiry" : "replay rejection check failed";
        } else if (expiryWaitSeconds) {
          report.naturalLoginExpiry = expiry ? "beyond configured wait budget" : "no cookie expiry exposed";
        }
        if (await connected()) await disconnect();
        progress("Checking browser and fresh-client rejection after removing the private login");
        await cdp.call("Network.clearBrowserCookies");
        const openedWithoutCookie = await evaluate(`new Promise(resolve => {
          const socket = new WebSocket(location.origin.replace('https:', 'wss:') + '/ws', 'binary');
          const timeout = setTimeout(() => { socket.close(); resolve(null); }, 8000);
          socket.onopen = () => { clearTimeout(timeout); socket.close(); resolve(true); };
          socket.onerror = () => { clearTimeout(timeout); resolve(false); };
          socket.onclose = () => { clearTimeout(timeout); resolve(false); };
        })`);
        result.browserUpgradeWithoutLogin = openedWithoutCookie === false ? "blocked" : openedWithoutCookie === true ? "opened" : "timeout";
        recordRejection("browserUpgradeWithoutLogin", openedWithoutCookie === false,
          "Browser WebSocket without login must be blocked");
        await navigate(route.origin);
        await until(async () => !await appReady(), "page denied after removing disposable login");
        await rejectionCheck("removedLogin");
        result.cookieRemovalIsNotNaturalExpiry = true;
      }
      await route.stop();
      if (!email) await until(async () => !await connected(), "shutdown disconnected viewer");
      await until(async () => { try { const r = await probe(route.origin, "/"); return r.status >= 500; } catch { return true; } }, "old public URL unavailable", 30_000);
      result.shutdown = true; save();
      progress(`Run ${run}: completed checks and closed route`);
    }
    report.completedImplementedChecks = !report.securityChecksFailed;
  } catch (error) {
    report.failedPhase = phase;
    if (error instanceof CheckFailure) report.failedCheck = error.message;
    const kind = error?.constructor?.name;
    report.failureKind = ["TypeError", "RangeError", "SyntaxError", "CheckFailure"].includes(kind)
      ? kind : "external error";
    // No raw exception: browser or service errors can contain credentials.
    process.exitCode = 1;
  } finally {
    report.awaitingOwnerLogin = false;
    progress("Cleaning up test-owned browser, connector, gateway, and fixture");
    if (cdp) { try { await cdp.call("Browser.close"); } catch {} cdp.ws.close(); }
    for (const p of [...children].reverse()) { try { await stop(p); } catch { report.cleanupFailed = true; } }
    if (rfb) {
      report.fixture = { connections: rfb.audit.connections, authenticated: rfb.audit.authenticated, rejected: rfb.audit.rejected,
        frames: rfb.audit.frames, keyDown: rfb.audit.keyDown, keyUp: rfb.audit.keyUp,
        pointerDown: rfb.audit.pointerDown, pointerUp: rfb.audit.pointerUp,
        unsupportedMessage: rfb.audit.unsupportedMessage,
        unsupportedPixelFormat: rfb.audit.unsupportedPixelFormat };
      await rfb.close();
    }
    report.remainingOwnedChildren = children.size;
    // Resolve and verify the recursive cleanup target before deleting only
    // this run's generated scratch directory. No user paths are enumerated.
    const clean = resolve(scratch);
    check(clean.startsWith(resolve(output) + sep), "Scratch cleanup must stay within this run");
    if (!children.size) {
      try { rmSync(clean, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 }); }
      catch { report.scratchCleanupFailed = true; }
    }
    if (children.size || report.scratchCleanupFailed) report.cleanupFailed = true;
    if (report.cleanupFailed) process.exitCode = 1;
    report.finished = new Date().toISOString(); save();
    process.removeListener("SIGINT", interrupt); process.removeListener("SIGTERM", interrupt);
    console.log(`Report: ${join(output, "report.json")}`);
    console.log(report.completedImplementedChecks && !report.cleanupFailed ? "Implemented live checks passed; see separately pending checks" : "Live checks incomplete or failed; see phase in report");
  }
  return report;
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) await run();
