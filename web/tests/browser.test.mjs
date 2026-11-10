// Built-page integration using a real Chromium browser and a controlled session
// Worker. Protocol/WASM and private gateway tests run separately; this checks
// the UI wiring, canvas ownership, pairing writes, and late-event fencing.
import test from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { readFileSync, existsSync, mkdtempSync, realpathSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, dirname, basename } from "node:path";
import { once } from "node:events";
import init, { newBrowserKey, publicKey } from "../build/pkg/topvnc_web.js";

await init({ module_or_path: readFileSync(new URL("../build/pkg/topvnc_web_bg.wasm", import.meta.url)) });
const executable = process.env.TOPVNC_TEST_BROWSER ?? [
  "C:/Program Files/Google/Chrome/Application/chrome.exe",
  "C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe",
  "/usr/bin/chromium", "/usr/bin/chromium-browser", "/usr/bin/google-chrome",
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
].find(existsSync);

test("built page switches machines, isolates pairing, clears secrets and fences late messages", { timeout: 30000 }, async (t) => {
  assert.ok(executable, "Set TOPVNC_TEST_BROWSER to a Chromium executable");
  const keys = [0, 1, 2].map(() => ({ host: publicKey(newBrowserKey()), browser: newBrowserKey() }));
  let metadata = { legacy: false, legacyHostId: null, hosts: [{ id: "gaming-pc", name: "Gaming PC" }, { id: "office-pc", name: "Office PC" }] };
  let delayDiagnostics = false;
  const files = { "/": ["index.html", "text/html"], "/app.js": ["app.js", "text/javascript"],
    "/style.css": ["style.css", "text/css"], "/topvnc_web_bg.wasm": ["topvnc_web_bg.wasm", "application/wasm"] };
  const server = createServer((req, res) => {
    const path = new URL(req.url, "http://localhost").pathname;
    if (path === "/api/hosts") { res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify(metadata)); return; }
    if (!files[path]) { res.writeHead(404); res.end(); return; }
    const [file, type] = files[path];
    res.setHeader("Content-Type", type);
    if (path === "/" && delayDiagnostics && req.headers["sec-fetch-dest"] === "empty") {
      setTimeout(() => res.end(readFileSync(new URL(`../dist/${file}`, import.meta.url))), 100);
      return;
    }
    res.end(readFileSync(new URL(`../dist/${file}`, import.meta.url)));
  });
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  t.after(() => { server.closeAllConnections(); server.close(); });
  const origin = `http://127.0.0.1:${server.address().port}`;
  const profile = realpathSync(mkdtempSync(join(tmpdir(), "topvnc-browser-test-")));
  const browser = spawn(executable, ["--headless=new", "--disable-gpu", "--no-first-run", "--no-default-browser-check",
    "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
  t.after(async () => {
    browser.kill();
    await once(browser, "exit").catch(() => {});
    assert.equal(dirname(profile), realpathSync(tmpdir()));
    assert.ok(basename(profile).startsWith("topvnc-browser-test-"));
    rmSync(profile, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  });
  const endpoint = await new Promise((resolve, reject) => {
    let stderr = "";
    const timer = setTimeout(() => reject(Error("Browser did not expose DevTools within 10 seconds")), 10000);
    browser.on("error", reject);
    browser.stderr.on("data", (data) => {
      stderr += data;
      const match = /DevTools listening on (ws:\/\/[^\s]+)/.exec(stderr);
      if (match) { clearTimeout(timer); resolve(match[1]); }
    });
    browser.on("exit", () => reject(new Error("Browser exited before exposing DevTools")));
  });
  const port = new URL(endpoint).port;
  const target = await (await fetch(`http://127.0.0.1:${port}/json/new?about:blank`, { method: "PUT" })).json();
  const ws = new WebSocket(target.webSocketDebuggerUrl);
  await once(ws, "open");
  t.after(() => ws.close());
  let sequence = 0;
  const pending = new Map();
  ws.addEventListener("message", (event) => {
    const message = JSON.parse(event.data);
    const entry = pending.get(message.id);
    if (entry) { pending.delete(message.id); message.error ? entry.reject(Error(message.error.message)) : entry.resolve(message.result); }
  });
  const command = (method, params = {}) => new Promise((resolve, reject) => {
    const id = ++sequence;
    const timer = setTimeout(() => { pending.delete(id); reject(Error(`DevTools timed out: ${method}`)); }, 3000);
    pending.set(id, { resolve(value) { clearTimeout(timer); resolve(value); }, reject(error) { clearTimeout(timer); reject(error); } });
    ws.send(JSON.stringify({ id, method, params }));
  });
  const evaluate = async (expression) => {
    const result = await command("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(result.exceptionDetails.exception?.description ?? result.exceptionDetails.text);
    return result.result.value;
  };
  const waitFor = async (expression) => {
    for (let i = 0; i < 200; i++) {
      if (await evaluate(expression)) return;
      await new Promise((resolve) => setTimeout(resolve, 10));
    }
    throw new Error(`Timed out: ${expression}`);
  };
  await command("Page.enable");
  await command("Page.addScriptToEvaluateOnNewDocument", { source: `
    window.fixture = { workers: [], messages: [] };
    const keys = ${JSON.stringify(keys)};
    localStorage.clear();
    if (location.search.includes('legacy')) {
      localStorage.setItem('topvnc.e2e.pairing.v1', JSON.stringify({ version: 1, ...keys[0] }));
    } else {
      ['gaming-pc', 'office-pc'].forEach((id, i) => localStorage.setItem('topvnc.e2e.pairing.v2.' + id, JSON.stringify({ version: 1, ...keys[i] })));
    }
    window.Worker = class {
      constructor() { fixture.workers.push(this); }
      emit(message) { this.onmessage?.({ data: message }); }
      postMessage(message) {
        fixture.messages.push({ ...message, canvas: undefined });
        if (message.type === 'connect') setTimeout(() => { this.emit({ type: 'verified' }); this.emit({ type: 'password' }); }, 0);
        if (message.type === 'password') setTimeout(() => { this.emit({ type: 'connected' }); this.emit({ type: 'resized', width: 800, height: 600 }); }, 0);
        if (message.type === 'close') setTimeout(() => this.emit({ type: 'closed', code: 4000, reason: 'Disconnected.' }), 20);
      }
      terminate() { this.terminated = true; }
    };
  ` });
  await command("Page.navigate", { url: origin });
  await waitFor("!document.querySelector('#connect')?.disabled");
  assert.equal(await evaluate("document.querySelector('#machine').value"), "gaming-pc");
  await evaluate("document.querySelector('#connect').click()");
  await waitFor("!document.querySelector('#password-form').hidden");
  await evaluate("document.querySelector('#password').value = 'ephemeral'; document.querySelector('#password-form').requestSubmit()");
  await waitFor("!document.querySelector('#viewer').hidden");
  await evaluate("fixture.workers[0].emit({ type: 'captureStatus', status: 2 })");
  assert.equal(await evaluate("!document.querySelector('#host-status').hidden && document.querySelector('#host-status').textContent.includes('lock screen')"), true);
  await evaluate(`
    fixture.oldCanvas = document.querySelector('#canvas');
    document.querySelector('.type-modifiers [data-mod="Ctrl"]').click();
    document.querySelector('#password').value = 'discard-me';
    const select = document.querySelector('#menu-machine'); select.value = 'office-pc'; select.dispatchEvent(new Event('change'));
  `);
  assert.equal(await evaluate("document.querySelector('#password').value"), "");
  assert.equal(await evaluate("document.querySelector('#host-status').hidden && document.querySelector('#host-status').textContent === ''"), true, "switching clears the previous machine's capture warning");
  await waitFor("!document.querySelector('#connect').disabled");
  assert.equal(await evaluate("fixture.workers[0].terminated && !fixture.oldCanvas.isConnected && document.querySelector('#canvas').width === 1"), true);
  assert.equal(await evaluate("fixture.messages.filter(m => m.type === 'connect').length"), 1, "switching does not auto-connect");
  assert.equal(await evaluate("fixture.messages.some(m => m.kind === 'release') && fixture.messages.some(m => m.type === 'close')"), true);
  await evaluate("document.querySelector('#connect').click()");
  await waitFor("!document.querySelector('#password-form').hidden");
  await evaluate(`fixture.workers[0].emit({ type: 'connected' }); fixture.workers[0].emit({ type: 'verified' }); fixture.workers[0].emit({ type: 'resized', width: 999, height: 999 }); fixture.workers[0].emit({ type: 'captureStatus', status: 1 });`);
  assert.equal(await evaluate("document.querySelector('#viewer').hidden && !document.querySelector('#password-form').hidden && document.querySelector('#canvas').width === 1"), true);
  assert.equal(await evaluate("fixture.messages.filter(m => m.type === 'connect')[1].url.endsWith('/ws/office-pc')"), true);
  await evaluate("document.querySelector('#password').value = 'office-password'; document.querySelector('#password-form').requestSubmit()");
  await waitFor("!document.querySelector('#viewer').hidden");
  assert.equal(await evaluate("document.querySelector('#host-status').hidden && document.querySelector('#host-status').textContent === ''"), true, "a healthy machine needs no capture-status event to clear the old warning");

  // End B's session, forget B, and enroll its new invite.
  await evaluate("document.querySelector('#machine').dispatchEvent(new Event('change'))");
  await waitFor("!document.querySelector('#forget').disabled");
  await evaluate("document.querySelector('#forget').click()");
  const invite = `v1.${keys[2].host}.${newBrowserKey()}`;
  await evaluate(`location.hash = 'pair=${invite}'`);
  await waitFor("document.querySelector('#identity').textContent.includes('Pairing invite')");
  await evaluate("document.querySelector('#connect').click()");
  await waitFor("!document.querySelector('#password-form').hidden");
  assert.equal(await evaluate("JSON.parse(localStorage.getItem('topvnc.e2e.pairing.v2.office-pc')).host"), keys[2].host);
  assert.equal(await evaluate("JSON.parse(localStorage.getItem('topvnc.e2e.pairing.v2.gaming-pc')).host"), keys[0].host);
  await evaluate(`history.replaceState(null, '', '/?host=unknown'); location.hash = 'pair=${invite}'`);
  await waitFor("document.querySelector('#status').textContent.includes('does not name a configured machine')");
  assert.equal(await evaluate("document.querySelector('#machine').value"), "office-pc");

  delayDiagnostics = true;
  await evaluate(`
    fixture.workers.at(-1).emit({ type: 'closed', code: 1006, reason: '', connected: false });
    { const select = document.querySelector('#machine'); select.value = 'gaming-pc'; select.dispatchEvent(new Event('change')); }
  `);
  await waitFor("!document.querySelector('#connect').disabled");
  await evaluate("new Promise(resolve => setTimeout(resolve, 150))");
  assert.equal(await evaluate("document.querySelector('#status').textContent"), "Ready for Gaming PC.");

  metadata = { legacy: true, legacyHostId: null, hosts: [{ id: "default", name: "This machine" }] };
  await command("Page.navigate", { url: `${origin}/?legacy` });
  await waitFor("!document.querySelector('#connect')?.disabled");
  await evaluate("document.querySelector('#connect').click()");
  await waitFor("!document.querySelector('#password-form').hidden");
  assert.equal(await evaluate("fixture.messages.find(m => m.type === 'connect').url.endsWith('/ws')"), true);
});
