// Shared offline session fixture for behavioral and visual browser checks.
import { build } from 'esbuild';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
const root = fileURLToPath(new URL('../../', import.meta.url));
export async function buildOfflineClient(dir) {
  mkdirSync(dir, { recursive: true });
  const bundle = await build({
    absWorkingDir: root, entryPoints: ['web/app.mjs'], write: false, bundle: true, format: 'iife', define: { 'import.meta.url': '"file:///offline/"' },
    plugins: [{ name: 'offline-session-fixture', setup(build) {
      build.onResolve({ filter: /build\/pkg\/topvnc_web\.js$/ }, () => ({ path: 'wasm-fixture', namespace: 'fixture' }));
      build.onLoad({ filter: /.*/, namespace: 'fixture' }, () => ({ contents: `export default async function init() {} export const fingerprint=()=> 'test-host', newBrowserKey=()=> 'A'.repeat(43), publicKey=()=> 'B'.repeat(43), parseInvite=()=>({free(){}}); export class Connection {}` }));
      build.onLoad({ filter: /web[\\/]app\.mjs$/ }, args => ({ contents: readFileSync(args.path, 'utf8') + `\nglobalThis.testConnect=async()=>{pending={browser:'A'.repeat(43),host:'B'.repeat(43)};state='connecting';await startSession();};globalThis.testSessionMessage=onSessionMessage;` }));
    } }],
  });
  // A reload in the placement test retains preferences; other fixture loads reset.
  const fixture = `try { if (!new URL(location.href).searchParams.has('preserveStorage')) localStorage.clear(); } catch {} globalThis.sent=[]; globalThis.Worker=class {
    postMessage(message) { if(message.type==='input') sent.push(message);
      if(message.type==='connect') queueMicrotask(()=>{
        if(message.canvas) { const c=message.canvas.getContext('2d'); message.canvas.width=1280;message.canvas.height=720;c.fillStyle='#e7e9e0';c.fillRect(0,0,1280,720);c.fillStyle='#fafbf6';c.fillRect(100,110,1080,450);c.strokeStyle='#adb59f';c.strokeRect(100,110,1080,450);c.fillStyle='#f3f5ed';c.fillRect(101,111,1078,50);c.fillStyle='#747e62';c.font='20px monospace';c.fillText('Offline layout fixture — no live host',125,145);c.font='28px monospace';c.fillText('Your remote workspace',145,240);c.font='20px monospace';c.fillText('The session controls below are the actual application.',145,290); }
        this.onmessage({data:{type:'connected',name:'Offline QA fixture'}});
        this.onmessage({data:{type:'resized',width:1280,height:720}});
      });
    }
  };`;
  const css = readFileSync(root + 'web/style.css', 'utf8').replace(/url\("(assets\/fonts\/[^"]+)"\)/gu, (_, path) => `url("data:font/woff2;base64,${readFileSync(root + 'web/' + path).toString('base64')}")`);
  let html = readFileSync(root + 'web/index.html', 'utf8').replace(/<script[^>]*src="\/app.js"[^>]*><\/script>/u, '').replace('<link rel="stylesheet" href="/style.css">', `<style>${css}</style>`);
  html = html.replace('</body>', () => `<script>${fixture}\n${bundle.outputFiles[0].text}</script></body>`);
  writeFileSync(dir + 'index.html', html);
}
