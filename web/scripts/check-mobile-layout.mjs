// Optional Chromium QA of the actual app, with an offline session fixture.
// No host credentials, network connection or local preview server.
import { build } from 'esbuild';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const root = fileURLToPath(new URL('../../', import.meta.url));
const dir = fileURLToPath(new URL('../build/mobile-qa/', import.meta.url));
const browser = process.env.TOPVNC_BROWSER;
if (!browser) throw new Error('Set TOPVNC_BROWSER to a Chromium/Chrome headless executable. This check uses an offline Worker fixture and starts no HTTP server.');
mkdirSync(dir, { recursive: true });
const bundle = await build({
  absWorkingDir: root, entryPoints: ['web/app.mjs'], write: false, bundle: true, format: 'iife', define: { 'import.meta.url': '"file:///offline/"' },
  plugins: [{ name: 'offline-session-fixture', setup(build) {
    build.onResolve({ filter: /build\/pkg\/topvnc_web\.js$/ }, () => ({ path: 'wasm-fixture', namespace: 'fixture' }));
    build.onLoad({ filter: /.*/, namespace: 'fixture' }, () => ({ contents: `export default async function init() {} export const fingerprint=()=> 'test-host', newBrowserKey=()=> 'A'.repeat(43), publicKey=()=> 'B'.repeat(43), parseInvite=()=>({free(){}}); export class Connection {}` }));
    build.onLoad({ filter: /web[\\/]app\.mjs$/ }, args => ({ contents: readFileSync(args.path, 'utf8') + `\nglobalThis.testConnect=async()=>{pending={browser:'A'.repeat(43),host:'B'.repeat(43)};state='connecting';await startSession();};` }));
  } }],
});
const fixture = `try { localStorage.clear(); } catch {} globalThis.sent=[]; globalThis.Worker=class {
  postMessage(message) { if(message.type==='input') sent.push(message);
    if(message.type==='connect') queueMicrotask(()=>{
      if(message.canvas) { const c=message.canvas.getContext('2d'); message.canvas.width=1280;message.canvas.height=720;c.fillStyle='#142b3a';c.fillRect(0,0,1280,720);c.fillStyle='#90b6af';c.font='36px monospace';c.fillText('Offline layout fixture',80,100); }
      this.onmessage({data:{type:'connected',name:'Offline QA fixture'}});
      this.onmessage({data:{type:'resized',width:1280,height:720}});
    });
  }
};`;
let html = readFileSync(root + 'web/index.html', 'utf8').replace(/<script[^>]*src="\/app.js"[^>]*><\/script>/u, '').replace('<link rel="stylesheet" href="/style.css">', `<style>${readFileSync(root + 'web/style.css', 'utf8')}</style>`);
html = html.replace('</body>', `<script>${fixture}\n${bundle.outputFiles[0].text}</script></body>`);
writeFileSync(dir + 'index.html', html);
const child = spawn(browser, ['--headless', '--remote-debugging-pipe', '--no-first-run', `--user-data-dir=${dir}profile`, 'about:blank'], { stdio: ['ignore', 'ignore', 'pipe', 'pipe', 'pipe'], windowsHide: true });
let seq = 0; let data = ''; const pending = new Map(); const errors = [];
child.stderr.on('data', () => {});
child.stdio[4].on('data', chunk => {
  data += chunk.toString(); let end;
  while ((end = data.indexOf('\0')) >= 0) {
    const message = JSON.parse(data.slice(0, end)); data = data.slice(end + 1);
    if (message.id) { const task = pending.get(message.id); pending.delete(message.id); message.error ? task?.reject(message.error) : task?.resolve(message.result); }
    else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails);
  }
});
function send(method, params = {}, sessionId) {
  return new Promise((resolve, reject) => { const id = ++seq; pending.set(id, { resolve, reject }); child.stdio[3].write(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }) + '\0'); });
}
child.on('error', error => { for (const task of pending.values()) task.reject(error); pending.clear(); });
const watchdog = setTimeout(() => { child.kill(); console.error('Browser QA timed out'); process.exitCode = 1; }, 45000);
try {
  const { targetInfos } = await send('Target.getTargets');
  const { sessionId } = await send('Target.attachToTarget', { targetId: targetInfos.find(t => t.type === 'page').targetId, flatten: true });
  const cdp = (method, params) => send(method, params, sessionId);
  await cdp('Runtime.enable'); await cdp('Page.enable');
  const evaluate = async expression => {
    const response = await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result.value;
  };
  await cdp('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await cdp('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  await cdp('Page.navigate', { url: pathToFileURL(dir + 'index.html').href });
  await evaluate(`new Promise(resolve=>{const check=()=>typeof testConnect==='function'?resolve():setTimeout(check,10);check();})`);
  await evaluate('testConnect()');
  await evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
  const click = selector => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
  const frame = () => evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
  const measure = () => evaluate(`(()=>{const ids=['screen','toolbar','control-deck'];return Object.fromEntries(ids.map(id=>{const r=document.getElementById(id).getBoundingClientRect();return[id,{x:r.x,y:r.y,width:r.width,height:r.height}]}));})()`);
  for (const size of [{ width: 390, height: 844 }, { width: 844, height: 390 }, { width: 320, height: 568 }]) {
    await cdp('Emulation.setDeviceMetricsOverride', { ...size, deviceScaleFactor: 1, mobile: true, screenOrientation: size.width > size.height ? { type: 'landscapePrimary', angle: 90 } : { type: 'portraitPrimary', angle: 0 } }); await frame();
    for (const mode of ['keys', 'type', 'pointer']) {
      await click(`[data-mode="${mode}"]`); await frame(); const boxes = await measure();
      assert.ok(boxes.screen.height >= 89, JSON.stringify({size,mode,boxes}));
      assert.ok(boxes.screen.width > 150, JSON.stringify({size,mode,boxes}));
      const dock = await evaluate(`[...document.querySelectorAll('.thumb-dock button')].map(n=>{let r=n.getBoundingClientRect();return {width:r.width,height:r.height,y:r.y,bottom:r.bottom};})`);
      assert.ok(dock.every(r => r.width >= 44 && r.height >= 44 && r.y >= 0 && r.bottom <= size.height + 1));
      if (mode === 'type') {
        assert.ok(await evaluate(`(()=>{const r=document.querySelector('#keyboard-languages').getBoundingClientRect(), dock=document.querySelector('.thumb-dock').getBoundingClientRect();return r.width>=44&&r.height>=44&&r.bottom<=dock.top;})()`));
      }
      if (mode === 'pointer') {
        assert.ok(await evaluate(`(()=>{const deck=document.querySelector('#control-deck').getBoundingClientRect();return [...document.querySelectorAll('.pointer-buttons button')].every(button=>{const r=button.getBoundingClientRect();return r.left>=deck.left&&r.right<=deck.right&&r.width>=44&&r.height>=44&&button.scrollWidth<=button.clientWidth;});})()`), `Pointer buttons overflow at ${size.width}x${size.height}`);
      }
      console.log(JSON.stringify({ size, mode, boxes }));
      const screenshot = await cdp('Page.captureScreenshot', { format: 'png' }); writeFileSync(`${dir}${size.width}-${mode}.png`, Buffer.from(screenshot.data, 'base64'));
    }
  }
  await click('[data-mode="type"]');
  await evaluate(`sent.length=0;document.querySelector('.type-modifiers [data-mod="Ctrl"]').click();document.querySelector('[data-character="c"]').click();`);
  assert.deepEqual(await evaluate('sent.map(m=>[m.keysym,m.down])'), [[65507,true],[99,true],[99,false],[65507,false]]);
  await evaluate(`sent.length=0;document.querySelector('.type-modifiers [data-mod="Ctrl"]').click();window.dispatchEvent(new KeyboardEvent('keydown',{code:'KeyC',key:'c',cancelable:true}));document.querySelector('#show-menu').dispatchEvent(new KeyboardEvent('keyup',{code:'KeyC',key:'c',bubbles:true,cancelable:true}));`);
  assert.deepEqual(await evaluate('sent.map(m=>[m.kind,m.keysym??m.code,m.down])'), [['keysym',65507,true],['code','KeyC',true],['code','KeyC',false],['keysym',65507,false]]);
  await click('#customize-button');
  await evaluate(`[...document.querySelectorAll('#sheet-content button')].find(b=>b.textContent==='Your keys').click();let f=document.querySelector('.custom-key-form');f.querySelector('input').value='Hello';f.querySelector('select').value='text';f.querySelector('select').dispatchEvent(new Event('change'));f.querySelector('textarea').value='ไทย 日本';f.dispatchEvent(new Event('submit',{bubbles:true,cancelable:true}));`);
  await click('#close-sheet');
  await evaluate(`sent.length=0;document.querySelector('#custom-action-pad button').click()`);
  assert.deepEqual(await evaluate(`sent.filter(m=>m.kind==='text').map(m=>m.text)`), ['ไทย 日本']);
  await click('#native-keyboard');
  await evaluate(`sent.length=0;let field=document.querySelector('#keyboard');field.dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true}));field.value+='日本';field.dispatchEvent(new CompositionEvent('compositionend',{bubbles:true}));field.dispatchEvent(new InputEvent('input',{bubbles:true,inputType:'insertText',data:'日本'}));`);
  assert.deepEqual(await evaluate(`sent.filter(m=>m.kind==='text').map(m=>m.text)`), ['日本']);
  // A resizes-content native keyboard changes viewport shape, not screen angle.
  await cdp('Emulation.setDeviceMetricsOverride', { width: 390, height: 240, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'portraitPrimary', angle: 0 } }); await frame();
  assert.equal(await evaluate(`document.activeElement.id`), 'keyboard');
  assert.ok(await evaluate(`document.querySelector('.thumb-dock').getBoundingClientRect().bottom <= innerHeight + 1`));
  await cdp('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'portraitPrimary', angle: 0 } }); await frame();
  await click('#custom-keyboard'); await click('#customize-button');
  await evaluate(`[...document.querySelectorAll('#sheet-content button')].find(b=>b.textContent.includes('Use FPS preset')).click()`);
  await frame();
  await cdp('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  const points = await evaluate(`['forward','fire'].map((action,id)=>{let r=document.querySelector('[data-fps-action="'+action+'"]').getBoundingClientRect();return{id,x:r.x+r.width/2,y:r.y+r.height/2};})`);
  await evaluate('sent.length=0');
  await cdp('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: [points[0]] });
  await cdp('Input.dispatchTouchEvent', { type: 'touchStart', touchPoints: points });
  assert.ok(await evaluate('sent.some(m=>m.kind==="keysym"&&m.keysym===119&&m.down)'));
  assert.ok(await evaluate('sent.some(m=>m.kind==="pointer"&&m.buttons===1)'));
  await cdp('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [points[0]] });
  assert.ok(await evaluate('sent.some(m=>m.keysym===119&&m.down===false)'));
  assert.equal(await evaluate('sent.filter(m=>m.kind==="pointer").at(-1).buttons'), 1);
  await cdp('Input.dispatchTouchEvent', { type: 'touchEnd', touchPoints: [] });
  assert.equal(await evaluate('sent.filter(m=>m.kind==="pointer").at(-1).buttons'), 0);
  await evaluate(`sent.length=0;document.querySelector('[data-fps-action="forward"]').dispatchEvent(new KeyboardEvent('keydown',{bubbles:true,key:'Enter'}));document.querySelector('[data-fps-action="fire"]').dispatchEvent(new KeyboardEvent('keydown',{bubbles:true,key:'Enter'}));window.dispatchEvent(new Event('blur'));`);
  assert.ok(await evaluate(`sent.some(m=>m.kind==='keysym'&&m.keysym===119&&m.down===false)`));
  assert.equal(await evaluate(`sent.at(-1).kind`), 'release');
  assert.deepEqual(errors, []);
  console.log('PASS: actual app layout, chord routing, native IME deduplication, multitouch and blur release (offline Worker fixture).');
} finally { clearTimeout(watchdog); await send('Browser.close').catch(() => {}); child.kill(); }
