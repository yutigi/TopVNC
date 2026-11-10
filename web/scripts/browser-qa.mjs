// Headless Chromium over its private pipe; no debugging port or HTTP server.
import { spawn } from 'node:child_process';

export async function browserQA(directory) {
  const executable = process.env.TOPVNC_BROWSER;
  if (!executable) throw new Error('Set TOPVNC_BROWSER to an installed Chromium executable.');
  const child = spawn(executable, ['--headless', '--remote-debugging-pipe', '--no-first-run',
    '--force-device-scale-factor=1', `--user-data-dir=${directory}profile`, 'about:blank'],
  { stdio: ['ignore', 'ignore', 'pipe', 'pipe', 'pipe'], windowsHide: true });
  let seq = 0, data = '';
  const pending = new Map(), errors = [];
  child.stderr.on('data', () => {});
  child.stdio[4].on('data', chunk => {
    data += chunk.toString();
    let end;
    while ((end = data.indexOf('\0')) >= 0) {
      const message = JSON.parse(data.slice(0, end)); data = data.slice(end + 1);
      if (message.id) {
        const task = pending.get(message.id); pending.delete(message.id);
        message.error ? task?.reject(new Error(JSON.stringify(message.error))) : task?.resolve(message.result);
      } else if (message.method === 'Runtime.exceptionThrown') errors.push(message.params.exceptionDetails);
    }
  });
  function rejectAll(error) { for (const task of pending.values()) task.reject(error); pending.clear(); }
  child.on('error', rejectAll);
  child.on('exit', () => rejectAll(new Error('QA browser exited before completing the command.')));
  const watchdog = setTimeout(() => { rejectAll(new Error('Visual QA timed out.')); child.kill(); }, 90000);
  function send(method, params = {}, sessionId) {
    return new Promise((resolve, reject) => {
      const id = ++seq; pending.set(id, { resolve, reject });
      child.stdio[3].write(JSON.stringify({ id, method, params, ...(sessionId ? { sessionId } : {}) }) + '\0');
    });
  }
  const { targetInfos } = await send('Target.getTargets');
  const { sessionId } = await send('Target.attachToTarget', { targetId: targetInfos.find(t => t.type === 'page').targetId, flatten: true });
  const cdp = (method, params) => send(method, params, sessionId);
  await cdp('Runtime.enable'); await cdp('Page.enable');
  await cdp('Emulation.setTouchEmulationEnabled', { enabled: true, maxTouchPoints: 5 });
  async function evaluate(expression) {
    const response = await cdp('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (response.exceptionDetails) throw new Error(JSON.stringify(response.exceptionDetails));
    return response.result.value;
  }
  return {
    cdp, evaluate, errors,
    frame: () => evaluate('new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r)))'),
    click: selector => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`),
    async close() { clearTimeout(watchdog); await send('Browser.close').catch(() => {}); child.kill(); },
  };
}
