// The README's screenshots ("Project" 9, "Cluster" 33): a throwaway cluster (qa/cluster.sh) given invented stacks
// through the API, each page taken by headless Chromium in the light theme (<page>.png) and the dark one
// (<page>-dark.png). qa/screenshots.sh runs it; by hand:
//
//   node qa/screenshots.mjs <base> <setup-token-file> <out-dir>
//
// A picture is refused (and the run fails) when its page says null, undefined or NaN, shows an error, or has a
// broken image. FD_CHROMIUM names the browser (default: the first of chromium-browser, chromium, google-chrome).
import { spawn, execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync, mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { randomBytes } from 'node:crypto';

const [base, setupFile, out] = process.argv.slice(2);
if (!base || !setupFile || !out) {
  console.error('usage: node qa/screenshots.mjs <base> <setup-token-file> <out-dir>');
  process.exit(2);
}
// The throwaway cluster's UI has a certificate of its own making, on 127.0.0.1 only.
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const WIDTH = 1440, HEIGHT = 900;

// --- the invented cluster, set up through the API as a person would
let cookie = '', csrf = '';
async function api(method, path, body) {
  const headers = { origin: base, cookie };
  if (body !== undefined) headers['content-type'] = 'application/json';
  if (method !== 'GET') headers['x-csrf-token'] = csrf;
  const r = await fetch(base + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const set = r.headers.getSetCookie?.() ?? [];
  if (set.length) cookie = set.map((c) => c.split(';')[0]).join('; ');
  const text = await r.text();
  if (r.status >= 300) throw new Error(`${method} ${path}: ${r.status} ${text}`);
  return text ? JSON.parse(text) : null;
}
const name = 'admin', password = randomBytes(18).toString('base64url');
await api('POST', '/api/setup', { name, password, token: readFileSync(setupFile, 'utf8').trim() });
await api('POST', '/api/login', { name, password });
csrf = (await api('GET', '/api/me')).csrf;

let nodes = [];
for (let i = 0; i < 60 && nodes[0]?.liveness?.health !== 'healthy'; i++) { nodes = await api('GET', '/api/cluster/nodes'); await sleep(1000); }
const node = nodes[0].id;
await api('PUT', '/api/cluster/name', { name: 'Example cluster' });
await api('PUT', `/api/cluster/nodes/${node}/name`, { name: 'node-1' });
const web = (replicas) => `services:\n  web:\n    image: nginx:alpine\n    deploy:\n      replicas: ${replicas}\n`;
await api('POST', '/api/cluster/stacks', { name: 'shop', compose: web(2), description: 'The web shop', env: 'TZ=UTC\n' });
await api('POST', '/api/cluster/stacks', { name: 'blog', compose: web(1), description: 'The blog' });
await api('POST', '/api/cluster/stacks', { name: 'wiki', compose: web(1), description: 'Made, not started yet', stopped: true });
await api('POST', '/api/users', { name: 'reader', password: randomBytes(18).toString('base64url'), role: 'user' });
for (let i = 0; i < 90; i++) {
  const all = await api('GET', '/api/cluster/stacks');
  const running = all.filter((s) => s.name !== 'wiki').every((s) => (s.workloads ?? []).length && s.workloads.every((w) => w.status === 'running'));
  if (running) break;
  await sleep(1000);
}

const pages = [
  ['dashboard', '/dashboard'],
  ['cluster', '/cluster'],
  ['stacks', '/cluster/stacks'],
  ['deploy', '/cluster/stacks/new'],
  ['node', `/cluster/nodes/${node}`],
  ['network', '/cluster/network'],
  ['integrations', '/integrations'],
  ['activity', '/activity'],
  ['users', '/users'],
  ['settings', '/settings'],
];

// --- the browser, over the DevTools protocol
const browser = process.env.FD_CHROMIUM || ['chromium-browser', 'chromium', 'google-chrome'].find((b) => { try { execFileSync('sh', ['-c', `command -v ${b}`]); return true; } catch { return false; } });
if (!browser) { console.error('no Chromium: set FD_CHROMIUM'); process.exit(2); }
const profile = mkdtempSync(join(tmpdir(), 'fd-shots-'));
const port = 9300 + Math.floor(Math.random() * 600);
const chrome = spawn(browser, ['--headless=new', `--remote-debugging-port=${port}`, '--ignore-certificate-errors', '--hide-scrollbars', `--window-size=${WIDTH},${HEIGHT}`, `--user-data-dir=${profile}`, 'about:blank'], { stdio: 'ignore' });
let target;
for (let i = 0; i < 50 && !target; i++) { await sleep(200); try { target = (await (await fetch(`http://127.0.0.1:${port}/json`)).json()).find((t) => t.type === 'page'); } catch { /* not up yet */ } }
const ws = new WebSocket(target.webSocketDebuggerUrl);
await new Promise((r) => (ws.onopen = r));
let seq = 0;
const waits = new Map();
ws.onmessage = (m) => { const d = JSON.parse(m.data); if (d.id && waits.has(d.id)) { waits.get(d.id)(d); waits.delete(d.id); } };
const cmd = (method, params = {}) => new Promise((r) => { const i = ++seq; waits.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
const run = async (expression) => (await cmd('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true })).result?.result?.value;
await cmd('Page.enable');
await cmd('Emulation.setDeviceMetricsOverride', { width: WIDTH, height: HEIGHT, deviceScaleFactor: 1, mobile: false });
await cmd('Page.navigate', { url: base + '/login' });
await sleep(1500);
const signedIn = await run(`fetch('/api/login', { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ name: ${JSON.stringify(name)}, password: ${JSON.stringify(password)} }) }).then((r) => r.status)`);
if (signedIn !== 200) { console.error('the browser could not sign in:', signedIn); process.exit(1); }

// What a picture must not show.
const faults = `(() => {
  const text = document.querySelector('main')?.innerText ?? document.body.innerText;
  const bad = [];
  const word = text.match(/\\b(null|undefined|NaN)\\b/);
  if (word) bad.push('says ' + word[0] + ': ...' + text.slice(Math.max(0, word.index - 60), word.index + 40).replace(/\\s+/g, ' ') + '...');
  for (const e of document.querySelectorAll('[role=alert]')) if (e.textContent.trim()) bad.push('shows an error: ' + e.textContent.trim().slice(0, 120));
  for (const i of document.images) if (i.complete && i.naturalWidth === 0 && i.getAttribute('src')) bad.push('broken image ' + i.getAttribute('src'));
  if (!document.querySelector('h1')) bad.push('has no heading');
  return bad;
})()`;

mkdirSync(out, { recursive: true });
let refused = 0;
for (const theme of ['light', 'dark']) {
  await run(`localStorage.setItem('freightdock.theme', '${theme}')`);
  await cmd('Emulation.setEmulatedMedia', { features: [{ name: 'prefers-color-scheme', value: theme }, { name: 'prefers-reduced-motion', value: 'reduce' }] });
  for (const [file, path] of pages) {
    await cmd('Page.navigate', { url: base + path });
    await sleep(3500);
    const bad = await run(faults);
    const shot = `${file}${theme === 'dark' ? '-dark' : ''}.png`;
    if (bad?.length) {
      refused++;
      console.log(`refused ${shot} (${path}): ${bad.join('; ')}`);
      continue;
    }
    const png = await cmd('Page.captureScreenshot', { format: 'png' });
    writeFileSync(join(out, shot), Buffer.from(png.result.data, 'base64'));
    console.log(`took    ${shot}`);
  }
}
ws.close();
await new Promise((r) => { chrome.once('exit', r); chrome.kill(); });
rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
console.log(refused ? `${refused} refused` : 'all taken');
process.exit(refused ? 1 : 0);
