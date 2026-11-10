// Goldens come ONLY from the accepted wireframe, never from the live client.
// Each run also renders that source afresh: changing a reference/font requires
// an explicit baseline refresh and source-hash review.
import { build } from 'esbuild';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { fileURLToPath, pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
import { buildOfflineClient } from './offline-client.mjs';
import { browserQA } from './browser-qa.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const dir = root + 'web/build/reference-qa/';
const goldenDir = root + 'web/tests/golden/mobile-reference/';
const update = process.argv.includes('--update-reference');
const probe = process.argv.includes('--probe-font-regression');
assert.ok(!(probe && update), 'A regression probe must never update baselines.');
mkdirSync(dir, { recursive: true });
if (update) mkdirSync(goldenDir, { recursive: true });
const read = path => readFileSync(root + path, 'utf8');
const referenceFiles = ['index.html', 'style.css', 'keyboard.css', 'customize.css', 'fps.css',
  'app.mjs', 'customize.mjs', 'fps.mjs', 'language-switcher.mjs'].map(name => 'design/topvnc-mobile-wireframe/dist/' + name);
const fontFiles = ['dm-sans-latin.woff2', 'ibm-plex-mono-latin.woff2'].map(name => 'web/assets/fonts/' + name);
const hash = path => createHash('sha256').update(path.endsWith('.woff2') ? readFileSync(root + path) : read(path).replace(/\r\n/gu, '\n')).digest('hex');
const provenance = Object.fromEntries([...referenceFiles, ...fontFiles].map(path => [path, hash(path)]));
const fonts = `@font-face{font-family:"DM Sans";font-style:normal;font-weight:400 700;src:url(data:font/woff2;base64,${readFileSync(root + fontFiles[0]).toString('base64')})}
@font-face{font-family:"IBM Plex Mono";font-style:normal;font-weight:400;src:url(data:font/woff2;base64,${readFileSync(root + fontFiles[1]).toString('base64')})}`;
const css = ['style.css', 'keyboard.css', 'customize.css', 'fps.css'].map(name => read('design/topvnc-mobile-wireframe/dist/' + name).replace(/^@import[^\r\n]+/mu, '')).join('\n');
const bundled = await build({
  stdin: { contents: read(referenceFiles[5]) + '\nglobalThis.referenceSetMode=setMode;globalThis.referenceOrientation=setOrientation;', resolveDir: root + 'design/topvnc-mobile-wireframe/dist' },
  bundle: true, write: false, format: 'iife',
});
// Remove only design-lab chrome and simulated device bezel. Keep every control's
// original CSS, DOM, text and interaction. The production framebuffer is separate.
const viewport = `body{margin:0}.workbench-header,.project-nav,.inspector,.canvas-heading,.stage-ruler,.preview-caption,.scenario-bar,.workbench-footer,.device-status,.home-indicator,.callout{display:none!important}
.workbench,.canvas-area,.preview-stage{display:contents!important}
#device.device{position:fixed;inset:0;width:100%;height:100dvh;max-width:none;border:0;border-radius:0;box-shadow:none;transition:none}`;
let reference = read(referenceFiles[0]).replace(/<link rel="stylesheet"[^>]+>/gu, '').replace(/<script type="module"[^>]+><\/script>/gu, '');
reference = reference.replace('</head>', `<style>${fonts}\n${css}\n${viewport}</style></head>`)
  .replace('</body>', () => `<script>try{localStorage.clear()}catch{}\n${bundled.outputFiles[0].text}</script></body>`);
writeFileSync(dir + 'reference.html', reference);
await buildOfflineClient(dir);
const baseline = update ? { provenance, cases: {} } : JSON.parse(readFileSync(goldenDir + 'manifest.json', 'utf8'));
assert.deepEqual(provenance, baseline.provenance, 'Reference/font hashes changed: inspect them before regenerating reference-only goldens.');
const props = ['fontFamily', 'fontSize', 'fontWeight', 'lineHeight', 'letterSpacing', 'color', 'backgroundColor',
  'borderTopColor', 'borderTopWidth', 'borderRadius', 'paddingTop', 'paddingRight', 'paddingBottom', 'paddingLeft', 'gap'];
const components = {
  keys: [['customize', '#customize-button'], ['pins', '.pin-row'], ['quick-controls', '.quick-controls'], ['dock', '.thumb-dock']],
  type: [['modifiers', '.type-modifiers'], ['source', '.type-toolbar', '.input-source'], ['draft', '.compose-wrap', '.draft-row'], ['compose', '#compose'], ['insert', '#insert-text'], ['keyboard', '#keyboard-rows'], ['keyboard-bottom', '.keyboard-bottom'], ['dock', '.thumb-dock']],
  pointer: [['trackpad', '#trackpad'], ['mouse-buttons', '.mouse-buttons', '.pointer-buttons'], ['dock', '.thumb-dock']],
};
const failures = [], report = [];
const browser = await browserQA(dir);
const { cdp, evaluate, click, frame } = browser;
try {
  if (update) baseline.renderer = (await cdp('Browser.getVersion')).product;
  for (const size of [{ width: 390, height: 844 }, { width: 844, height: 390 }]) {
    await cdp('Emulation.setDeviceMetricsOverride', { ...size, deviceScaleFactor: 1, mobile: true,
      screenOrientation: size.width > size.height ? { type: 'landscapePrimary', angle: 90 } : { type: 'portraitPrimary', angle: 0 } });
    for (const mode of ['keys', 'type', 'pointer']) {
      const key = `${size.width}-${mode}`;
      const expected = {};
      for (const kind of ['reference', 'actual']) {
        await cdp('Page.navigate', { url: pathToFileURL(dir + (kind === 'reference' ? 'reference.html' : 'index.html')).href });
        await evaluate(`new Promise(resolve=>{const check=()=>typeof ${kind === 'reference' ? 'referenceSetMode' : 'testConnect'}==='function'?resolve():setTimeout(check,10);check()})`);
        if (kind === 'reference') await evaluate(`referenceOrientation('${size.width > size.height ? 'landscape' : 'portrait'}');referenceSetMode('${mode}')`);
        else { await evaluate('testConnect()'); await click(`[data-mode="${mode}"]`); }
        if (kind === 'actual' && probe) await evaluate(`(()=>{const s=document.createElement('style');s.textContent='#control-deck button{font-weight:700!important}';document.head.append(s)})()`);
        // The real trackpad explains independent mouse holds; the prototype
        // labels its simulated tap behavior. Mask only that intentional copy.
        await evaluate(`document.querySelector('#trackpad small').style.visibility='hidden'`);
        await evaluate('document.fonts.ready'); await frame();
        // No offline page should reach out to Google or any session host.
        assert.deepEqual(await evaluate('performance.getEntriesByType("resource").filter(r=>/^https?:/.test(r.name)).map(r=>r.name)'), []);
        const shot = await cdp('Page.captureScreenshot', { format: 'png' });
        writeFileSync(`${dir}${key}-${kind}.png`, Buffer.from(shot.data, 'base64'));
        for (const [name, referenceSelector, actualSelector = referenceSelector] of components[mode]) {
          const selector = kind === 'reference' ? referenceSelector : actualSelector;
          // Deliberate accessibility correction: production retains 44px touch
          // targets where the prototype's landscape pins/mouse buttons are smaller.
          if (kind === 'reference' && size.width > size.height && ['pins', 'mouse-buttons'].includes(name)) {
            await evaluate(`document.querySelectorAll('${selector} button').forEach(n=>{n.style.minHeight='44px';n.style.height='44px'})`);
          }
          const metrics = await evaluate(`(()=>{const n=document.querySelector(${JSON.stringify(selector)}),r=n.getBoundingClientRect(),deck=document.querySelector('${kind === 'reference' ? '.control-deck' : '#control-deck'}').getBoundingClientRect(),s=getComputedStyle(n),b=getComputedStyle(n.querySelector('button')??n);return{width:r.width,height:r.height,offset:{left:r.left-deck.left,top:r.top-deck.top},styles:Object.fromEntries(${JSON.stringify(props)}.map(k=>[k,s[k]])),button:Object.fromEntries(${JSON.stringify(props)}.map(k=>[k,b[k]]))}})()`);
          const file = `${key}-${name}.png`;
          const width = metrics.width;
          // Isolate each control at an integer origin so device scroll clipping,
          // sticky-dock overlap and fractional offsets cannot corrupt a golden.
          await evaluate(`(()=>{const n=document.querySelector(${JSON.stringify(selector)});globalThis.savedGoldenStyle=n.getAttribute('style');const bg=getComputedStyle(n).backgroundColor;Object.assign(n.style,{position:'fixed',left:'0',top:'0',width:'${width}px',height:'${metrics.height}px',margin:'0',zIndex:'10000',backgroundColor:bg==='rgba(0, 0, 0, 0)'?'white':bg})})()`);
          await frame();
          const { data } = await cdp('Page.captureScreenshot', { format: 'png', clip: { x: 0, y: 0, width, height: metrics.height, scale: 1 } });
          await evaluate(`(()=>{const n=document.querySelector(${JSON.stringify(selector)});savedGoldenStyle===null?n.removeAttribute('style'):n.setAttribute('style',savedGoldenStyle)})()`);
          await frame();
          writeFileSync(`${dir}${kind}-${file}`, Buffer.from(data, 'base64'));
          if (kind === 'reference') {
            expected[name] = { metrics, data };
            if (update) {
              baseline.cases[file] = metrics;
              writeFileSync(goldenDir + file, Buffer.from(data, 'base64'));
            }
            continue;
          }
          const expectedMetrics = baseline.cases[file];
          try { assert.deepEqual(expected[name].metrics, expectedMetrics); } catch { failures.push(`${file}: reference geometry/styles differ from its saved baseline`); }
          for (const group of ['styles', 'button']) for (const property of props) {
            if (metrics[group][property] !== expectedMetrics[group][property]) failures.push(`${file}: ${group}.${property}: ${metrics[group][property]} (reference ${expectedMetrics[group][property]})`);
          }
          for (const dimension of ['width', 'height']) if (Math.abs(metrics[dimension] - expectedMetrics[dimension]) > 1) failures.push(`${file}: ${dimension} ${metrics[dimension]} (reference ${expectedMetrics[dimension]})`);
          // Portrait Keys/Type should also preserve the reference's spacing,
          // rather than passing isolated screenshots with incorrect gaps.
          if ((size.width < size.height && mode !== 'pointer' && name !== 'customize') || (mode === 'type' && name !== 'dock')) {
            for (const axis of ['left', 'top']) if (Math.abs(metrics.offset[axis] - expectedMetrics.offset[axis]) > 1) failures.push(`${file}: ${axis} offset ${metrics.offset[axis]} (reference ${expectedMetrics.offset[axis]})`);
          }
          const golden = readFileSync(goldenDir + file).toString('base64');
          const comparison = await evaluate(`(async()=>{
            const load=async data=>createImageBitmap(await(await fetch('data:image/png;base64,'+data)).blob());
            const a=await load(${JSON.stringify(data)}),b=await load(${JSON.stringify(golden)});
            if(a.width!==b.width||a.height!==b.height)return{differentSize:true,actual:[a.width,a.height],expected:[b.width,b.height]};
            const c=new OffscreenCanvas(a.width,a.height),ctx=c.getContext('2d');
            ctx.drawImage(a,0,0);const p=ctx.getImageData(0,0,a.width,a.height).data;
            ctx.drawImage(b,0,0);const q=ctx.getImageData(0,0,b.width,b.height).data;
            const diff=ctx.createImageData(a.width,a.height);let changed=0;
            for(let i=0;i<p.length;i+=4){const different=Math.max(...[0,1,2].map(k=>Math.abs(p[i+k]-q[i+k])))>16;if(different)changed++;diff.data.set(different?[220,35,85,255]:[q[i],q[i+1],q[i+2],70],i)}
            ctx.putImageData(diff,0,0);const blob=await c.convertToBlob({type:'image/png'});
            const png=await new Promise(resolve=>{const f=new FileReader();f.onload=()=>resolve(f.result.split(',')[1]);f.readAsDataURL(blob)});
            return{changed:changed/(a.width*a.height),diff:png};
          })()`);
          if (comparison.diff) { writeFileSync(`${dir}diff-${file}`, Buffer.from(comparison.diff, 'base64')); delete comparison.diff; }
          report.push({ file, ...comparison });
          // Exact styles and <=1px geometry guard against design drift; allow
          // up to 2% raster differences for small text/antialiasing on Chromium.
          if (comparison.differentSize || comparison.changed > .02) failures.push(`${file}: pixels ${JSON.stringify(comparison)}`);
        }
      }
    }
  }
  if (update) writeFileSync(goldenDir + 'manifest.json', JSON.stringify(baseline, null, 2) + '\n');
  writeFileSync(dir + 'report.json', JSON.stringify({ failures, comparisons: report }, null, 2) + '\n');
  // Keep the review page current with the public placement setting as well as
  // the unchanged reference comparisons. These extra shots are not goldens.
  await cdp('Emulation.setDeviceMetricsOverride', { width: 844, height: 390, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'landscapePrimary', angle: 90 } });
  await cdp('Page.navigate', { url: pathToFileURL(dir + 'index.html').href });
  await evaluate(`new Promise(resolve=>{const check=()=>typeof testConnect==='function'?resolve():setTimeout(check,10);check()})`);
  await evaluate('testConnect()'); await evaluate('document.fonts.ready');
  for (const side of ['left', 'right']) {
    await click('#show-menu');
    await evaluate(`(()=>{const s=document.querySelector('#landscape-controls');s.value='${side}';s.dispatchEvent(new Event('change',{bubbles:true}))})()`);
    await click('#close-menu');
    for (const mode of ['keys', 'type', 'pointer']) {
      await click(`[data-mode="${mode}"]`); await frame();
      const shot = await cdp('Page.captureScreenshot', { format: 'png' });
      writeFileSync(`${dir}844-${side}-${mode}-actual.png`, Buffer.from(shot.data, 'base64'));
    }
  }
  await cdp('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true, screenOrientation: { type: 'portraitPrimary', angle: 0 } });
  await click('#show-menu'); await frame();
  const settingsShot = await cdp('Page.captureScreenshot', { format: 'png' });
  writeFileSync(dir + '390-settings-actual.png', Buffer.from(settingsShot.data, 'base64'));
  const rows = [390, 844].flatMap(width => ['keys', 'type', 'pointer'].map(mode => `<section><h2>${width}px · ${mode}</h2><div class="pair"><figure><figcaption>Accepted reference</figcaption><img src="${width}-${mode}-reference.png"></figure><figure><figcaption>Implemented client · offline session</figcaption><img src="${width}-${mode}-actual.png"></figure></div></section>`));
  const placements = ['keys', 'type', 'pointer'].map(mode => `<section><h2>Landscape placement · ${mode}</h2><div class="pair">${['left', 'right'].map(side => `<figure><figcaption>${side === 'left' ? 'Left' : 'Right'} side · saved in Settings</figcaption><img src="844-${side}-${mode}-actual.png"></figure>`).join('')}</div></section>`);
  writeFileSync(dir + 'comparison.html', `<!doctype html><meta charset="utf-8"><title>TopVNC reference comparison</title><style>body{margin:32px;background:#eeefea;color:#262824;font:16px system-ui}h1{font-size:24px}.pair{display:flex;gap:20px;flex-wrap:wrap}figure{margin:0}figcaption{margin:12px 0;color:#555c43}img{max-width:100%;border:1px solid #dedfd8}section{margin:32px 0}</style><h1>Reference and implemented mobile controls</h1><p>Updated ${new Date().toISOString()}. Production security copy and the simulated framebuffer differ intentionally. Golden comparisons cover controls, portrait spacing and the stacked Type rows in landscape.</p>${rows.join('')}<section><h2>Session settings</h2><p>Landscape controls changes apply immediately and persist across reloads. Portrait keeps its own layout.</p><img src="390-settings-actual.png" alt="Session settings with the Landscape controls selector"></section>${placements.join('')}`);
  assert.deepEqual(browser.errors, [], 'Browser exceptions during reference comparison');
  if (probe) {
    assert.ok(failures.some(f => f.includes('button.fontWeight')), 'The deliberate font-weight regression was not detected.');
    assert.ok(failures.some(f => f.includes('pixels')), 'The deliberate font-weight regression did not fail pixel comparison.');
    console.log(`PASS: deliberate bold-font regression detected by styles and pixels (${failures.length} failures).`);
  } else {
    assert.equal(failures.length, 0, failures.join('\n'));
    console.log(`PASS: ${report.length} reference-derived screenshot goldens and typography/geometry checks.`);
  }
} finally { await browser.close(); }
