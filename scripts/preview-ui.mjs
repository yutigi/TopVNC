// Local, read-only access to the existing interactive design prototypes.
import { createServer } from 'node:http';
import { readFile, realpath } from 'node:fs/promises';
import { extname, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('../', import.meta.url));
const port = Number(process.env.TOPVNC_UI_PORT || 4173);
if (!Number.isInteger(port) || port < 1 || port > 65535) {
  throw new Error('TOPVNC_UI_PORT must be an integer between 1 and 65535.');
}
const directories = new Map(await Promise.all(['desktop', 'mobile'].map(async name => [
  name, await realpath(resolve(root, `design/topvnc-${name}-wireframe/dist`)),
])));
const types = {
  '.html': 'text/html; charset=utf-8', '.css': 'text/css; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8', '.js': 'text/javascript; charset=utf-8',
  '.woff2': 'font/woff2', '.txt': 'text/plain; charset=utf-8',
  '.svg': 'image/svg+xml', '.png': 'image/png', '.ico': 'image/x-icon',
};
const home = `<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="theme-color" content="#f5f5f2"><title>TopVNC · Local design lab</title>
<style>
@font-face{font-family:DM;src:url('/desktop/assets/fonts/dm-sans-latin.woff2') format('woff2');font-display:swap}
*{box-sizing:border-box}body{margin:0;background:#f5f5f2;color:#262824;font-family:DM,system-ui,sans-serif}
main{max-width:1000px;margin:auto;padding:56px 28px}header{display:flex;justify-content:space-between;gap:20px;align-items:center;padding-bottom:28px;border-bottom:1px solid #dedfd8}
.brand{font-size:18px;font-weight:750;letter-spacing:2px}.badge{font-size:12px;background:#e9ecdf;color:#555c43;padding:9px 14px;border-radius:30px}
.eyebrow{margin-top:66px;color:#797c74;font-size:11px;letter-spacing:2px}h1{font-size:clamp(40px,7vw,68px);letter-spacing:-3px;line-height:1.05;font-weight:550;margin:20px 0}
.intro{color:#797c74;line-height:1.7;max-width:560px}.designs{display:grid;grid-template-columns:1fr 1fr;gap:20px;margin:40px 0}
a.card{display:flex;flex-direction:column;min-height:260px;padding:30px;border:1px solid #dedfd8;border-radius:18px;background:white;text-decoration:none;color:inherit;transition:transform .15s,border-color .15s}
a.card:hover{transform:translateY(-3px);border-color:#555c43}a:focus-visible{outline:3px solid #555c43;outline-offset:5px}.number{font-family:monospace;color:#797c74;font-size:12px}
h2{font-size:27px;font-weight:550;letter-spacing:-1px;margin:24px 0 10px}.card p{color:#797c74;line-height:1.7;margin:0 0 24px}.action{margin-top:auto;color:#555c43;font-weight:650}
footer{border-top:1px solid #dedfd8;padding-top:22px;color:#797c74;font-size:13px;line-height:1.8}code{font-size:12px;color:#555c43}
@media(max-width:600px){main{padding:28px 20px}.designs{grid-template-columns:1fr}.eyebrow{margin-top:42px}h1{letter-spacing:-2px}}
</style></head><body><main>
<header><span class="brand">↗ TOPVNC</span><span class="badge">Local design lab</span></header>
<p class="eyebrow">EXPLORE / TRY / REFINE</p><h1>A workspace you<br>can get your hands on.</h1>
<p class="intro">Explore the new desktop and mobile interfaces. Each preview is interactive, with a simulated remote workspace. No host or sign-in needed.</p>
<section class="designs" aria-label="Choose a design">
<a class="card" href="/desktop/"><span class="number">01 / DESKTOP</span><h2>Room for your workspace.</h2><p>Type in the demo editor. Explore session settings, display sizing, picture quality and full viewport.</p><span class="action">Open desktop preview ↗</span></a>
<a class="card" href="/mobile/"><span class="number">02 / MOBILE</span><h2>Built for your thumbs.</h2><p>Try portrait and landscape, multilingual typing, custom shortcuts, the trackpad and FPS touch controls.</p><span class="action">Open mobile preview ↗</span></a>
</section><footer>Prototype only · All sessions and remote input are simulated.<br>Runs on this computer. Changes to prototype preferences stay in this browser.</footer>
</main></body></html>`;

const server = createServer(async (request, response) => {
  const reply = (status, body, type = 'text/plain; charset=utf-8') => {
    response.writeHead(status, {
      'Content-Type': type, 'Cache-Control': 'no-store',
      'X-Content-Type-Options': 'nosniff', 'Referrer-Policy': 'no-referrer',
    });
    response.end(request.method === 'HEAD' ? undefined : body);
  };
  if (!['GET', 'HEAD'].includes(request.method)) {
    response.setHeader('Allow', 'GET, HEAD');
    return reply(405, 'Method not allowed');
  }
  try {
    const pathname = decodeURIComponent(new URL(request.url, 'http://localhost').pathname);
    if (pathname === '/') return reply(200, home, types['.html']);
    if (pathname === '/favicon.ico') return reply(204, '');
    const [, design, ...parts] = pathname.split('/');
    const directory = directories.get(design);
    if (!directory || parts.some(part => part === '..' || /[\\\0]/u.test(part))) {
      return reply(404, 'Not found');
    }
    if (parts.length === 0) {
      response.writeHead(302, { Location: `/${design}/` });
      return response.end();
    }
    const path = await realpath(resolve(directory, parts.join('/') || 'index.html'));
    if (!path.startsWith(directory + sep) || !types[extname(path)]) return reply(404, 'Not found');
    reply(200, await readFile(path), types[extname(path)]);
  } catch (error) {
    if (error instanceof URIError) return reply(400, 'Invalid URL');
    if (['ENOENT', 'ENOTDIR', 'EISDIR'].includes(error.code)) return reply(404, 'Not found');
    console.error('Preview request failed:', error.message);
    reply(500, 'Preview request failed');
  }
});
server.on('error', error => {
  console.error(error.code === 'EADDRINUSE'
    ? `Port ${port} is already in use. Set TOPVNC_UI_PORT to another port and try again.`
    : error.message);
  process.exitCode = 1;
});
server.listen(port, '127.0.0.1', () => {
  console.log(`TopVNC local design lab: http://127.0.0.1:${port}/`);
  console.log('Desktop: /desktop/ · Mobile: /mobile/ · Simulated sessions only');
  console.log('Press Ctrl+C to stop.');
});
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => server.close());
