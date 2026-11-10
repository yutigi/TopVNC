// Optional Chromium QA of the actual app, with an offline session fixture.
// No host credentials, network connection or local preview server.
import { buildOfflineClient } from './offline-client.mjs';
import { writeFileSync, mkdirSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const dir = fileURLToPath(new URL('../build/mobile-qa/', import.meta.url));
const browser = process.env.TOPVNC_BROWSER;
if (!browser) throw new Error('Set TOPVNC_BROWSER to a Chromium/Chrome headless executable. This check uses an offline Worker fixture and starts no HTTP server.');
mkdirSync(dir, { recursive: true });
await buildOfflineClient(dir);
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
  // A background/headless page can update activeElement without focus events.
  // Native text input deliberately requires a fresh focus after cancellation.
  await cdp('Emulation.setFocusEmulationEnabled', { enabled: true });
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
  await evaluate('document.fonts.ready');
  await evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
  const click = selector => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
  const frame = () => evaluate('new Promise(resolve=>requestAnimationFrame(()=>requestAnimationFrame(resolve)))');
  const checkSettings = async label => {
    await evaluate(`testSessionMessage({type:'capabilities',serveScale:0,displays:[]})`);
    await click('#show-menu'); await frame();
    assert.equal(await evaluate('document.activeElement.id'), 'close-menu', label);
    assert.ok(await evaluate(`(()=>{let m=document.querySelector('#menu'),r=m.getBoundingClientRect();return !m.hidden&&m.getAttribute('aria-modal')==='true'&&r.top>=document.querySelector('#toolbar').getBoundingClientRect().bottom-1&&r.bottom<=innerHeight+1;})()`),label);
    assert.ok(await evaluate(`document.querySelector('#serve-display').hidden&&document.querySelector('#serve-scale').hidden&&!document.querySelector('#serve-display-missing').hidden&&!document.querySelector('#serve-scale-missing').hidden&&document.querySelector('#serve-scale-value').textContent==='Unavailable'`),label);
    await evaluate(`testSessionMessage({type:'capabilities',serveScale:100,displays:[1,2]});sent.length=0;var scale=document.querySelector('#serve-scale');scale.value='75';scale.dispatchEvent(new Event('input',{bubbles:true}));scale.dispatchEvent(new Event('change',{bubbles:true}));var display=document.querySelector('[name="serve-display"][value="2"]');display.checked=true;display.dispatchEvent(new Event('change',{bubbles:true}));var q=document.querySelector('[name="menu-quality"][value="8"]');q.checked=true;q.dispatchEvent(new Event('change',{bubbles:true}));`);
    assert.deepEqual(await evaluate('sent.map(m=>[m.kind,m.percent??m.display??m.quality])'),[['serveScale',75],['serveDisplay',2],['quality',8]],label);
    assert.ok(await evaluate(`!document.querySelector('#serve-scale').hidden&&!document.querySelector('#serve-display').hidden&&document.querySelector('#serve-scale-value').textContent==='75% shared'&&document.querySelector('#serve-display-missing').hidden&&document.querySelector('#serve-scale-missing').hidden`),label);
    await evaluate(`testSessionMessage({type:'capabilities',serveScale:75,displays:[2,2]});testSessionMessage({type:'stats',fps:60,presentedFps:59,mbps:12,rtt:8,continuous:true})`);
    await click('#stats-toggle');
    assert.ok(await evaluate(`!document.querySelector('#stats').hidden&&document.querySelector('#stats').textContent.includes('RTT 8 ms')&&document.querySelector('#menu .transport').textContent.includes('Encrypted')&&document.querySelector('#fullscreen-menu').getClientRects().length>0`),label);
    const shot=await cdp('Page.captureScreenshot',{format:'png'});writeFileSync(`${dir}${label}-settings.png`,Buffer.from(shot.data,'base64'));
    await evaluate(`document.querySelector('#disconnect').focus()`);
    await cdp('Input.dispatchKeyEvent',{type:'keyDown',key:'Tab',code:'Tab',windowsVirtualKeyCode:9});
    assert.equal(await evaluate('document.activeElement.id'),'close-menu',label);
    await cdp('Input.dispatchKeyEvent',{type:'keyDown',key:'Tab',code:'Tab',windowsVirtualKeyCode:9,modifiers:8});
    assert.equal(await evaluate('document.activeElement.id'),'disconnect',label);
    await click('#stats-toggle');
    // Focus can leave the dialog (a click on its background): keys must not reach the host, and Escape still closes.
    await evaluate(`document.activeElement.blur();sent.length=0`);
    const typeW = async () => { for (const type of ['keyDown','keyUp']) await cdp('Input.dispatchKeyEvent',{type,code:'KeyW',key:'w',windowsVirtualKeyCode:87}); };
    await typeW();
    assert.equal(await evaluate('sent.length'),0,label);
    await cdp('Input.dispatchKeyEvent',{type:'keyDown',key:'Escape',code:'Escape',windowsVirtualKeyCode:27});
    assert.ok(await evaluate(`document.querySelector('#menu').hidden&&document.activeElement.id==='screen'&&!document.querySelector('#screen').inert&&document.querySelector('#stats').hidden&&document.querySelector('#show-menu').getAttribute('aria-expanded')==='false'`),label);
    // Focus returns to the remote screen it came from, so hardware keys reach the host again.
    await typeW();
    assert.deepEqual(await evaluate(`sent.filter(m=>m.kind==='code').map(m=>[m.code,m.down])`),[['KeyW',true],['KeyW',false]],label);
    // Settings keeps reset-to-fit reachable even when the phone toolbar hides Fit.
    await evaluate(`(()=>{sent.length=0;const r=document.querySelector('#screen').getBoundingClientRect();document.querySelector('#screen').dispatchEvent(new WheelEvent('wheel',{bubbles:true,cancelable:true,ctrlKey:true,deltaY:-100,clientX:r.left+20,clientY:r.top+20}));})()`);
    assert.notEqual(await evaluate(`document.querySelector('#canvas').style.transform`),'translate(0px, 0px) scale(1)',label);
    await click('#show-menu');
    await evaluate(`document.querySelector('#fit-menu').scrollIntoView({block:'center'})`); await frame();
    const fitButton = await evaluate(`(()=>{const b=document.querySelector('#fit-menu'),r=b.getBoundingClientRect();return {x:r.x+r.width/2,y:r.y+r.height/2,reachable:document.elementFromPoint(r.x+r.width/2,r.y+r.height/2)===b};})()`);
    assert.ok(fitButton.reachable,label);
    await cdp('Input.dispatchMouseEvent',{type:'mousePressed',x:fitButton.x,y:fitButton.y,button:'left',buttons:1,clickCount:1});
    await cdp('Input.dispatchMouseEvent',{type:'mouseReleased',x:fitButton.x,y:fitButton.y,button:'left',buttons:0,clickCount:1});
    assert.equal(await evaluate(`document.querySelector('#canvas').style.transform`),'translate(0px, 0px) scale(1)',label);
    assert.ok(await evaluate(`document.querySelector('#menu').hidden&&!document.querySelector('#screen').inert&&document.activeElement.id==='screen'`),label);
    assert.ok(await evaluate(`sent.every(m=>m.kind==='release')`),label);
    await evaluate(`testSessionMessage({type:'capabilities',serveScale:0,displays:[]})`);
  };
  const measure = () => evaluate(`(()=>{const ids=['screen','toolbar','control-deck'];return Object.fromEntries(ids.map(id=>{const r=document.getElementById(id).getBoundingClientRect();return[id,{x:r.x,y:r.y,width:r.width,height:r.height}]}));})()`);
  const checkPointerFill = async () => {
    const boxes = await evaluate(`(()=>{const box=s=>{const r=document.querySelector(s).getBoundingClientRect();return{top:r.top,bottom:r.bottom,height:r.height}};return{groups:box('.control-groups'),panel:box('#pointer-panel'),pad:box('#trackpad'),buttons:box('.pointer-buttons'),dock:box('.thumb-dock')};})()`);
    assert.ok(Math.abs(boxes.panel.bottom - boxes.groups.bottom) <= 1, `Unused Pointer space: ${JSON.stringify(boxes)}`);
    assert.ok(Math.abs(boxes.pad.bottom + 6 - boxes.buttons.top) <= 1, JSON.stringify(boxes));
    assert.ok(Math.abs(boxes.buttons.bottom - boxes.panel.bottom) <= 1 && boxes.buttons.bottom <= boxes.dock.top, JSON.stringify(boxes));
  };
  for (const size of [{ width: 390, height: 844 }, { width: 844, height: 390 }, { width: 320, height: 568 }, { width: 1440, height: 900 }, { width: 1024, height: 768 }, { width: 899, height: 700 }]) {
    await cdp('Emulation.setDeviceMetricsOverride', { ...size, deviceScaleFactor: 1, mobile: true, screenOrientation: size.width > size.height ? { type: 'landscapePrimary', angle: 90 } : { type: 'portraitPrimary', angle: 0 } }); await frame();
    await checkSettings(`${size.width}-touch`);
    for (const mode of ['keys', 'type', 'pointer']) {
      await click(`[data-mode="${mode}"]`); await frame(); const boxes = await measure();
      const screenshot = await cdp('Page.captureScreenshot', { format: 'png' }); writeFileSync(`${dir}${size.width}-touch-${mode}.png`, Buffer.from(screenshot.data, 'base64'));
      const minimumScreen = mode === 'type' && size.width > size.height && size.width >= 600 ? 54 : 89;
      assert.ok(boxes.screen.height >= minimumScreen, JSON.stringify({size,mode,boxes}));
      assert.ok(boxes.screen.width > 150, JSON.stringify({size,mode,boxes}));
      const dock = await evaluate(`[...document.querySelectorAll('.thumb-dock button')].map(n=>{let r=n.getBoundingClientRect();return {width:r.width,height:r.height,y:r.y,bottom:r.bottom};})`);
      assert.ok(dock.every(r => r.width >= 44 && r.height >= 44 && r.y >= 0 && r.bottom <= size.height + 1), JSON.stringify({size,mode,dock,boxes}));
      if (mode === 'type') {
        // Compact landscape typing follows the reference's 31px character keys.
        const keyHeight = size.width > size.height && size.width >= 600 ? 31 : 44;
        // On short viewports the reference stacks more rows than fit at once.
        // Keep the dock fixed and verify that the bottom keys can scroll into view.
        await evaluate(`document.querySelector('#keyboard-languages').scrollIntoView({block:'nearest'})`); await frame();
        assert.ok(await evaluate(`(()=>{const r=document.querySelector('#keyboard-languages').getBoundingClientRect(), dock=document.querySelector('.thumb-dock').getBoundingClientRect();return r.width>=44&&r.height>=${keyHeight}&&r.bottom<=dock.top;})()`));
        await evaluate(`document.querySelector('.control-groups').scrollTop=0`); await frame();
        assert.ok(await evaluate(`(()=>{const r=document.querySelector('#insert-text').getBoundingClientRect(),deck=document.querySelector('#control-deck').getBoundingClientRect();return r.width>=48&&r.height>=29&&r.top>=deck.top&&r.bottom<=deck.bottom;})()`), `Insert not visible at ${size.width}x${size.height}`);
      }
      if (mode === 'pointer') {
        await checkPointerFill();
        assert.ok(await evaluate(`(()=>{const deck=document.querySelector('#control-deck').getBoundingClientRect();return [...document.querySelectorAll('.pointer-buttons button')].every(button=>{const r=button.getBoundingClientRect();return r.left>=deck.left&&r.right<=deck.right&&r.width>=44&&r.height>=44&&button.scrollWidth<=button.clientWidth;});})()`), `Pointer buttons overflow at ${size.width}x${size.height}`);
      }
      console.log(JSON.stringify({ size, mode, boxes }));
    }
  }
  // Exercise the public setting, its persistence, and both actual side layouts.
  await cdp('Emulation.setDeviceMetricsOverride', { width: 844, height: 390, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'landscapePrimary', angle: 90 } });
  await click('[data-mode="keys"]'); await frame();
  for (const side of ['left', 'right']) {
    await click('#show-menu');
    await evaluate(`(()=>{const select=document.querySelector('#landscape-controls');select.value='${side}';select.dispatchEvent(new Event('change',{bubbles:true}))})()`);
    await click('#close-menu'); await frame();
    for (const mode of ['keys', 'pointer', 'type']) {
      await click(`[data-mode="${mode}"]`); await frame();
      const boxes = await measure(); const deck = boxes['control-deck'];
      assert.equal(deck.width, 310);
      assert.equal(deck.x, side === 'left' ? 0 : 534);
      assert.equal(boxes.screen.x, side === 'left' ? 310 : 0);
      assert.equal(boxes.screen.width, 534);
      if (mode === 'pointer') await checkPointerFill();
      assert.ok(await evaluate(`(()=>{const s=getComputedStyle(document.querySelector('#control-deck'));return s.border${side === 'left' ? 'Right' : 'Left'}Width==='1px'&&s.border${side === 'left' ? 'Left' : 'Right'}Width==='0px';})()`));
      const shot = await cdp('Page.captureScreenshot', { format: 'png' });
      writeFileSync(`${dir}844-${side}-${mode}.png`, Buffer.from(shot.data, 'base64'));
    }
    await cdp('Page.navigate', { url: pathToFileURL(dir + 'index.html').href + '?preserveStorage' });
    await evaluate(`new Promise(resolve=>{const check=()=>typeof testConnect==='function'?resolve():setTimeout(check,10);check()})`);
    await evaluate('testConnect()'); await evaluate('document.fonts.ready'); await frame();
    assert.equal(await evaluate(`document.querySelector('#landscape-controls').value`), side);
    assert.equal(await evaluate(`document.querySelector('#viewer').dataset.layoutPosition`), side);
    await cdp('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'portraitPrimary', angle: 0 } }); await frame();
    assert.equal(await evaluate(`document.querySelector('#viewer').dataset.layoutPosition`), 'auto');
    assert.equal((await measure())['control-deck'].width, 390);
    await cdp('Emulation.setDeviceMetricsOverride', { width: 844, height: 390, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'landscapePrimary', angle: 90 } }); await frame();
    assert.equal(await evaluate(`document.querySelector('#viewer').dataset.layoutPosition`), side);
  }
  await click('#show-menu');
  await evaluate(`(()=>{const select=document.querySelector('#landscape-controls');select.value='auto';select.dispatchEvent(new Event('change',{bubbles:true}))})()`);
  await click('#close-menu'); await frame();
  // Desktop occupies the full workspace, including short/portrait windows and
  // saved modes/placements. Width alone must not hide touch tablet controls.
  await cdp('Emulation.setTouchEmulationEnabled', { enabled: false });
  for (const size of [{width:1440,height:900},{width:1024,height:768},{width:1100,height:500},{width:1000,height:1100},{width:900,height:700}]) {
    await cdp('Emulation.setDeviceMetricsOverride', {...size,deviceScaleFactor:1,mobile:false}); await frame();
    assert.equal(await evaluate('matchMedia("(pointer: fine) and (hover: hover)").matches'), true);
    await checkSettings(`${size.width}-desktop`);
    for (const mode of ['keys','type','pointer']) {
      await click(`[data-mode="${mode}"]`);
      for (const position of ['auto','top','bottom','left','right']) {
        await evaluate(`document.querySelector('#viewer').dataset.layoutPosition=${JSON.stringify(position)}`); await frame();
        const boxes = await measure();
        assert.ok(Math.abs(boxes.screen.width-size.width)<1 && boxes.screen.x===0 && boxes.screen.y===64 && Math.abs(boxes.screen.height-(size.height-64))<1, JSON.stringify({size,mode,position,boxes}));
        assert.equal(boxes['control-deck'].width,0);
        assert.ok(await evaluate(`[...document.querySelectorAll('#control-deck, .thumb-dock, #customize-button, #show-keyboard, #fps-panel')].every(n=>n.getClientRects().length===0)`));
      }
    }
    // A saved FPS preset must not expose an overlay or reserve a dock gap.
    await evaluate(`document.querySelector('#viewer').classList.add('fps-active')`); await frame();
    assert.ok(await evaluate(`(()=>{let r=document.querySelector('#screen').getBoundingClientRect();return r.width===innerWidth&&r.bottom===innerHeight&&document.querySelector('#control-deck').getClientRects().length===0;})()`));
    const shot=await cdp('Page.captureScreenshot',{format:'png'}); writeFileSync(`${dir}${size.width}-desktop.png`,Buffer.from(shot.data,'base64'));
    await evaluate(`document.querySelector('#viewer').classList.remove('fps-active')`);
  }
  // Actual mouse events focus the remote surface and route hardware keys/buttons.
  await cdp('Input.dispatchMouseEvent',{type:'mousePressed',x:300,y:220,button:'left',buttons:1,clickCount:1});
  await cdp('Input.dispatchMouseEvent',{type:'mouseReleased',x:300,y:220,button:'left',buttons:0,clickCount:1});
  assert.equal(await evaluate('document.activeElement.id'),'screen');
  await evaluate('sent.length=0');
  await cdp('Input.dispatchKeyEvent',{type:'keyDown',code:'KeyW',key:'w',windowsVirtualKeyCode:87});
  await cdp('Input.dispatchMouseEvent',{type:'mousePressed',x:300,y:220,button:'left',buttons:1,clickCount:1});
  assert.ok(await evaluate(`sent.some(m=>m.kind==='code'&&m.code==='KeyW'&&m.down)&&sent.some(m=>m.kind==='pointer'&&m.buttons===1)`));
  await evaluate(`window.dispatchEvent(new Event('blur'))`);
  assert.equal(await evaluate('sent.at(-1).kind'),'release');
  await cdp('Input.dispatchMouseEvent',{type:'mouseReleased',x:300,y:220,button:'left',buttons:0,clickCount:1});
  await click('#show-menu');
  assert.equal(await evaluate('document.querySelector("#menu").hidden'),false);
  assert.ok(await evaluate('document.querySelector("#toolbar").textContent.includes("Encrypted")'));
  await click('#close-menu'); await click('#fit');
  // Changing input capability must release a hardware hold as the deck returns.
  await evaluate('document.querySelector("#screen").focus();sent.length=0');
  await cdp('Input.dispatchKeyEvent',{type:'keyDown',code:'KeyW',key:'w',windowsVirtualKeyCode:87});
  await cdp('Emulation.setTouchEmulationEnabled',{enabled:true,maxTouchPoints:5}); await frame();
  assert.ok(await evaluate('sent.some(m=>m.kind==="release")'));
  await evaluate(`document.querySelector('#viewer').dataset.layoutPosition='auto'`);
  await cdp('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'portraitPrimary', angle: 0 } }); await frame();
  // The pinned Fn shows its open state; with Fn unpinned the footnote still opens the F-key row.
  await click('[data-mode="keys"]'); await click('#show-function-keys');
  assert.ok(await evaluate(`(()=>{const b=document.querySelector('#show-function-keys');return !document.querySelector('#function-keys').hidden&&b.getAttribute('aria-expanded')==='true'&&getComputedStyle(b).backgroundColor==='rgb(105, 122, 80)';})()`));
  await click('#show-function-keys'); await click('#edit-pins');
  await evaluate(`document.querySelectorAll('#sheet-content select')[3].value='Ctrl+Z';[...document.querySelectorAll('#sheet-content button')].find(b=>b.textContent==='Save pinned keys').click();`);
  await click('#footnote-functions');
  assert.ok(await evaluate(`!document.querySelector('#function-keys').hidden&&document.querySelector('#footnote-functions').getClientRects().length>0&&!document.querySelector('#show-function-keys')`));
  await click('#footnote-functions');
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
  // Type hides the deck heading, so its status appears over the remote view.
  await evaluate(`document.querySelector('#keyboard').dispatchEvent(new CompositionEvent('compositionstart',{bubbles:true}));document.querySelector('[data-mode="keys"]').click();`);
  assert.ok(await evaluate(`(()=>{const t=document.querySelector('#control-toast'),r=t.getBoundingClientRect(),s=document.querySelector('#screen').getBoundingClientRect();return t.getClientRects().length>0&&t.textContent.includes('Finish composing')&&r.top>=s.top&&r.bottom<=s.bottom;})()`));
  await evaluate(`document.querySelector('#keyboard').dispatchEvent(new CompositionEvent('compositionend',{bubbles:true}))`);
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
  await click('#disconnect');
  assert.equal(await evaluate('sent.at(-1).kind'), 'release');
  assert.deepEqual(errors, []);
  console.log('PASS: desktop without touch deck, mobile layouts, hardware input, chord routing, native IME deduplication, multitouch and lifecycle release (offline Worker fixture).');
} finally { clearTimeout(watchdog); await send('Browser.close').catch(() => {}); child.kill(); }
