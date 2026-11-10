// Browser verification for the separate desktop prototype. No server or host.
import { buildSync } from '../../web/node_modules/esbuild/lib/main.js';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const base = fileURLToPath(new URL('./', import.meta.url));
const output = base + '.sites-runtime/qa/'; mkdirSync(output, { recursive: true });
if (!process.env.TOPVNC_BROWSER) throw new Error('Set TOPVNC_BROWSER to an installed Chromium executable.');
const bundle = name => buildSync({ entryPoints: [base + 'dist/' + name], bundle: true, write: false, format: 'iife' }).outputFiles[0].text;
const css = ['style.css','keyboard.css','customize.css','fps.css','desktop.css'].map(name => readFileSync(base + 'dist/' + name, 'utf8')).join('\n').replace(/url\("(assets\/fonts\/[^"]+)"\)/gu, (_, path) => `url("data:font/woff2;base64,${readFileSync(base + 'dist/' + path).toString('base64')}")`);
let html = readFileSync(base + 'dist/index.html', 'utf8').replace(/<link rel="stylesheet"[^>]+>/gu, '').replace(/<script[^>]*src="[^"]+"[^>]*><\/script>/gu, '');
html = html.replace('</head>', () => `<style>${css}</style></head>`).replace('</body>', () => `<script>try { localStorage.clear(); } catch {}\n${bundle('app.mjs')}\n${bundle('desktop.mjs')}</script></body>`);
writeFileSync(output + 'index.html', html);
const child = spawn(process.env.TOPVNC_BROWSER, ['--headless','--remote-debugging-pipe','--no-first-run',`--user-data-dir=${output}profile`,'about:blank'], { stdio: ['ignore','ignore','pipe','pipe','pipe'], windowsHide: true });
let sequence = 0; let data = ''; const pending = new Map(); const errors = [];
child.stderr.on('data', () => {});
child.stdio[4].on('data', chunk => {
  data += chunk.toString(); let end;
  while ((end = data.indexOf('\0')) >= 0) {
    const message = JSON.parse(data.slice(0,end)); data = data.slice(end + 1);
    if (message.id) { const task = pending.get(message.id); pending.delete(message.id); message.error ? task?.reject(message.error) : task?.resolve(message.result); }
    else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails);
  }
});
const send = (method, params = {}, sessionId) => new Promise((resolve,reject) => { const id = ++sequence; pending.set(id,{resolve,reject}); child.stdio[3].write(JSON.stringify({id,method,params,...(sessionId ? {sessionId} : {})}) + '\0'); });
child.on('error', error => { for (const task of pending.values()) task.reject(error); pending.clear(); });
const watchdog = setTimeout(() => { child.kill(); process.exitCode = 1; }, 45000);
try {
  const { targetInfos } = await send('Target.getTargets');
  const { sessionId } = await send('Target.attachToTarget', {targetId:targetInfos.find(t=>t.type==='page').targetId,flatten:true});
  const cdp = (method,params) => send(method,params,sessionId);
  await cdp('Runtime.enable'); await cdp('Page.enable'); await cdp('Emulation.setFocusEmulationEnabled',{enabled:true});
  await cdp('Emulation.setEmulatedMedia',{features:[{name:'prefers-reduced-motion',value:'reduce'}]});
  const evaluate = async expression => { const r = await cdp('Runtime.evaluate',{expression,returnByValue:true,awaitPromise:true}); if(r.exceptionDetails) throw new Error(JSON.stringify(r.exceptionDetails)); return r.result.value; };
  const frame = () => evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
  const click = selector => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
  const screenshot = async name => { await frame(); const r = await cdp('Page.captureScreenshot',{format:'png'}); writeFileSync(output + name + '.png',Buffer.from(r.data,'base64')); };
  await cdp('Emulation.setDeviceMetricsOverride',{width:1440,height:1050,deviceScaleFactor:1,mobile:false});
  await cdp('Page.navigate',{url:pathToFileURL(output + 'index.html').href});
  await evaluate('document.fonts.ready'); await frame();
  assert.deepEqual(errors, []);
  for(const width of [1440,1200,800]) {
    await cdp('Emulation.setDeviceMetricsOverride',{width,height:1050,deviceScaleFactor:1,mobile:false});
    for(const mode of ['keys','type','pointer']) {
      await click(`[data-mode="${mode}"]`); await frame();
      for(const position of ['left','right','top','bottom']) {
        await evaluate(`document.querySelector('#device').dataset.layoutPosition=${JSON.stringify(position)}`); await frame();
        const fits = await evaluate(`(()=>{const body=document.querySelector('.session-body').getBoundingClientRect(),r=document.querySelector('#remote').getBoundingClientRect(),deck=document.querySelector('.control-deck');return {remote:r.width,body:body.width,full:Math.abs(r.left-body.left)<1&&Math.abs(r.right-body.right)<1&&Math.abs(r.height-body.height)<1,hidden:deck.getClientRects().length===0,overflow:document.documentElement.scrollWidth>innerWidth};})()`);
        assert.ok(fits.remote>200&&fits.full&&fits.hidden&&!fits.overflow,JSON.stringify({width,mode,position,fits}));
      }
      await screenshot(`${width}-${mode}`);
    }
  }
  await click('[data-desktop-size="compact"]'); await screenshot('compact');
  assert.equal(await evaluate('document.body.dataset.desktopSize'),'compact');
  await click('[data-desktop-size="wide"]');
  await click('#desktop-fullscreen'); await screenshot('full-desktop');
  assert.ok(await evaluate(`(()=>{let r=document.querySelector('#device').getBoundingClientRect();return r.x===0&&r.y===0&&Math.abs(r.height-innerHeight)<1&&Math.abs(r.width-innerWidth)<1;})()`));
  await evaluate(`document.querySelector('#remote-output').textContent='';document.querySelector('#remote-output').focus()`);
  await cdp('Input.insertText',{text:'Hello ไทย 日本'});
  assert.equal(await evaluate(`document.querySelector('#remote-output').textContent`),'Hello ไทย 日本');
  for (const size of [{width:1440,height:900},{width:390,height:844},{width:844,height:390}]) {
    await cdp('Emulation.setDeviceMetricsOverride',{...size,deviceScaleFactor:1,mobile:false}); await frame();
    await click('#settings-button');
    assert.equal(await evaluate('document.activeElement.id'),'close-sheet');
    assert.ok(await evaluate(`document.querySelector('#settings-button').textContent.includes('Settings')&&document.querySelector('#sheet-content').textContent.includes('Simulated · not connected')`));
    assert.ok(await evaluate(`(()=>{let r=document.querySelector('#sheet').getBoundingClientRect();return r.left>=0&&r.right<=innerWidth+1&&r.top>=64&&r.bottom<=innerHeight;})()`));
    await evaluate(`var scale=document.querySelector('#demo-serve-scale');scale.value='60';scale.dispatchEvent(new Event('input'));var display=document.querySelector('#demo-serve-display');display.value='2';display.dispatchEvent(new Event('change'));document.querySelector('[name="demo-quality"][value="8"]').click()`);
    assert.ok(await evaluate(`document.querySelector('#demo-serve-scale-value').textContent.includes('60% · 1152 × 648')`));
    await screenshot(`${size.width}-settings`);
    await click('#demo-stats-toggle');
    assert.ok(await evaluate(`!document.querySelector('#demo-stats').hidden&&document.querySelector('#demo-stats').textContent.includes('SIMULATED DIAGNOSTICS')`));
    await evaluate(`document.querySelector('#demo-stats-toggle').scrollIntoView({block:'center'})`); await screenshot(`${size.width}-debug-settings`);
    await evaluate(`var caps=document.querySelector('#demo-capabilities');caps.value='unavailable';caps.dispatchEvent(new Event('change'))`);
    assert.ok(await evaluate(`document.querySelector('#demo-serve-scale').disabled&&document.querySelector('#demo-serve-display').disabled&&document.querySelector('#demo-serve-scale-value').textContent==='Unavailable'&&document.querySelector('#sheet-content').textContent.includes('Not offered by this host')`));
    await evaluate(`var caps=document.querySelector('#demo-capabilities');caps.value='offered';caps.dispatchEvent(new Event('change'))`);
    assert.ok(await evaluate(`!document.querySelector('#demo-serve-scale').disabled&&document.querySelector('#demo-serve-scale').value==='60'&&document.querySelector('#demo-serve-display').value==='2'&&document.querySelector('[name="demo-quality"]:checked').value==='8'`));
    await evaluate(`document.querySelector('#demo-disconnect').focus()`);
    await cdp('Input.dispatchKeyEvent',{type:'keyDown',key:'Tab',code:'Tab',windowsVirtualKeyCode:9});
    assert.equal(await evaluate('document.activeElement.id'),'close-sheet');
    await click('#demo-disconnect');
    assert.ok(await evaluate(`!document.querySelector('#demo-session-message').hidden&&document.querySelector('#demo-stats').hidden&&document.querySelector('#remote-output').contentEditable==='false'`));
    await click('#settings-button'); await click('#demo-disconnect');
    assert.ok(await evaluate(`document.querySelector('#demo-session-message').hidden&&!document.querySelector('#demo-stats').hidden`));
    await click('#settings-button'); await click('#demo-stats-toggle');
    await cdp('Input.dispatchKeyEvent',{type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    assert.ok(await evaluate(`document.querySelector('#sheet').hidden&&document.activeElement.id==='settings-button'`));
  }
  await click('#settings-button'); await click('#demo-fullscreen');
  assert.equal(await evaluate(`document.body.classList.contains('focus-mode')`),false);
  await click('#desktop-fullscreen');
  await click('#desktop-fit');
  assert.equal(await evaluate('document.activeElement.id'),'remote-output');
  await click('#exit-focus');
  // Simulate a retained FPS preset: the scene fills the body without touch UI.
  await click('#fps-preset-button'); await screenshot('fps');
  assert.equal(await evaluate(`document.querySelector('#device').classList.contains('fps-active')`),true);
  assert.ok(await evaluate(`(()=>{const r=document.querySelector('#remote').getBoundingClientRect(),b=document.querySelector('.session-body').getBoundingClientRect();return Math.abs(r.width-b.width)<1&&Math.abs(r.height-b.height)<1&&document.querySelector('.control-deck').getClientRects().length===0;})()`));
  assert.deepEqual(errors,[]);
  console.log('PASS: full-width desktop/laptop display, no rail/dock/overlay in retained modes and layouts, compact/full viewport, physical typing, settings, focus and FPS geometry.');
} finally { clearTimeout(watchdog); await send('Browser.close').catch(()=>{}); child.kill(); }
