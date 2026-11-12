// The screenshots in docs/screenshots/, from a throwaway Freightdock that qa/screenshots.sh started: everything shown is
// set up here through Freightdock's own API, as a person would through the UI, then each picture is taken in a real
// browser driven over the DevTools protocol (Node's own WebSocket: no npm, no Puppeteer, as the UI has no build step).
//
// To add a picture: add an entry to PAGES (the page, its size, what must be on screen first) and reference
// docs/screenshots/<name>.png from the docs. A picture is refused when its page says null, undefined or NaN, still
// names Shipshow, or does not show what it is about.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawn } from 'node:child_process';

const env = (k) => process.env[k] || (() => { throw new Error(`${k} is not set: run this through qa/screenshots.sh`); })();
const BASE = env('SHOTS_BASE'), OUT = env('SHOTS_OUT'), DIND = env('SHOTS_DIND');
const ONLY = (process.env.SHOTS_ONLY || '').split(',').map((s) => s.trim()).filter(Boolean);
const BROWSER = process.env.QA_BROWSER || ['/usr/bin/chromium', '/usr/bin/google-chrome-stable', '/usr/bin/brave', '/usr/bin/brave-origin'].find((p) => fs.existsSync(p));
const wait = (ms) => new Promise((r) => setTimeout(r, ms));
const say = (s) => console.error(`shots: ${s}`);

// ---- the API, as the signed-in browser calls it: the session cookie, the CSRF token and this server's Origin
let cookie = '', csrf = '';
async function api(method, url, body) {
  const r = await fetch(BASE + url, {
    method, headers: { cookie, 'x-csrf-token': csrf, origin: BASE, 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${url}: ${r.status} ${text}`);
  // A long job (a pull, a deploy) answers NDJSON and ends with {"done":true} or an error line.
  if ((r.headers.get('content-type') || '').includes('ndjson')) {
    const lines = text.trim().split('\n').map((l) => JSON.parse(l));
    const last = lines.at(-1) || {};
    if (!last.done) throw new Error(`${method} ${url} did not finish: ${JSON.stringify(last)}`);
    return lines;
  }
  return text ? JSON.parse(text) : null;
}
const get = (u) => api('GET', u), post = (u, b) => api('POST', u, b), put = (u, b) => api('PUT', u, b);
async function until(what, ok, seconds = 120) {
  for (let i = 0; i < seconds * 2; i++) {
    if (await ok().catch(() => false)) return;
    await wait(500);
  }
  throw new Error(`waited ${seconds} s for ${what}`);
}
const dind = (host, ...args) => execFileSync('docker', ['exec', `${DIND}-${host}`, 'docker', ...args], { encoding: 'utf8' }).trim();

// ---- what the pictures show: invented hosts, containers and a stack
async function setUp() {
  await until('Freightdock to answer', async () => (await fetch(BASE + '/api/health')).ok, 60);
  const r = await fetch(BASE + '/api/setup', {
    method: 'POST', headers: { origin: BASE, 'content-type': 'application/json' },
    body: JSON.stringify({ name: 'admin', password: 'a throwaway password for the pictures', token: env('SHOTS_SETUP_TOKEN') }),
  });
  if (!r.ok) throw new Error(`setup: ${r.status} ${await r.text()}`);
  cookie = (r.headers.getSetCookie().find((c) => c.startsWith('freightdock_session=')) || '').split(';')[0];
  if (!cookie) throw new Error('setup set no session cookie');
  csrf = (await get('/api/me')).csrf;
  const status = await get('/api/status');

  const local = (await get('/api/environments')).find((e) => e.kind === 'local');
  const on = (e) => `/api/envs/${e.id}`;
  say('the containers and the stack on local');
  const run = (name, config, more = {}) => post(`${on(local)}/containers`, { name, config, ...more });
  await run('cache', { Image: 'redis:7-alpine', HostConfig: { RestartPolicy: { Name: 'unless-stopped' } } });
  await run('db', {
    Image: 'postgres:16-alpine', Env: ['POSTGRES_PASSWORD=an-invented-password'],
    Healthcheck: { Test: ['CMD-SHELL', 'pg_isready -U postgres'], Interval: 2e9, Timeout: 2e9, Retries: 10 },
    HostConfig: { RestartPolicy: { Name: 'unless-stopped' } },
  });
  await run('web', { Image: 'nginx:alpine', ExposedPorts: { '80/tcp': {} }, HostConfig: { PortBindings: { '80/tcp': [{ HostPort: '8080' }] }, RestartPolicy: { Name: 'unless-stopped' } } });
  await run('local-registry', { Image: 'registry:2', ExposedPorts: { '5000/tcp': {} }, HostConfig: { PortBindings: { '5000/tcp': [{ HostPort: '5000' }] }, RestartPolicy: { Name: 'unless-stopped' } } });
  await run('nightly-job', { Image: 'busybox:1.37', Cmd: ['sh', '-c', 'echo the nightly job ran'] });
  const compose = [
    'services:',
    '  whoami:',
    '    image: traefik/whoami:latest',
    '    ports: ["8081:80"]',
    '  cache:',
    '    image: redis:7-alpine',
    '',
  ].join('\n');
  const stack = await post(`${on(local)}/stacks`, { name: 'whoami', compose });
  await post(`${on(local)}/stacks/${stack.id}/deploy`, {});
  await put(`/api/environments/${local.id}`, { tags: ['demo'] });

  const hosts = {};
  for (const [n, tags] of [['node-1', ['arm', 'edge']], ['node-2', ['edge']]]) {
    say(`${n}: an agent, enrolled with its join token`);
    const made = await post('/api/environments', { name: n, kind: 'agent' });
    hosts[n] = made.environment;
    dind(n, 'run', '-d', '--name', 'freightdock-agent', '--restart', 'unless-stopped', '-v', '/var/run/docker.sock:/var/run/docker.sock',
      '-v', 'freightdock-agent:/data', '-e', `FREIGHTDOCK_SERVER=${env('SHOTS_AGENT_SERVER')}`, '-e', `FREIGHTDOCK_JOIN_TOKEN=${made.join.token}`,
      env('SHOTS_AGENT_IMAGE'));
    await until(`${n}'s agent to come up`, async () => (await get(`/api/environments/${made.environment.id}`)).status === 'up');
    await put(`/api/environments/${made.environment.id}`, { tags });
    // Through the agent's tunnel, as any container on that host is made.
    await post(`${on(made.environment)}/containers`, { name: 'proxy', config: { Image: 'nginx:alpine', HostConfig: { RestartPolicy: { Name: 'unless-stopped' } } } });
    await post(`${on(made.environment)}/containers`, { name: 'cache', config: { Image: 'redis:7-alpine', HostConfig: { RestartPolicy: { Name: 'unless-stopped' } } } });
  }
  say('docker-tls: a host on Docker\'s TLS port');
  const pem = (f) => fs.readFileSync(path.join(env('SHOTS_TLS_DIR'), `${f}.pem`), 'utf8');
  const tls = await post('/api/environments', { name: 'docker-tls', kind: 'tls', url: env('SHOTS_TLS_URL'), tls_ca: pem('ca'), tls_cert: pem('cert'), tls_key: pem('key') });
  hosts['docker-tls'] = tls.environment;
  await put(`/api/environments/${tls.environment.id}`, { tags: ['tls'] });
  await post('/api/environment-groups', { name: 'Lab', description: 'Machines to try things on', environments: Object.values(hosts).map((e) => e.id) });

  say('waiting for every host to be up with its numbers, and db to be healthy');
  await until('every host up with a summary', async () => {
    const all = await get('/api/environments');
    return all.length === 4 && all.every((e) => e.status === 'up' && e.snapshot);
  }, 180);
  await until('db to be healthy', async () => (await get(`${on(local)}/containers`)).some((c) => /healthy/.test(c.Status) && c.Names.includes('/db') && !/unhealthy|starting/.test(c.Status)), 120);
  return { local, stack, version: status.version };
}

// ---- the pictures: which page, at which size, and what must be on screen before it is taken. Each is taken twice, in
// the light theme as <name>.png and the dark one as <name>-dark.png, so a page that shows one (the README, through
// <picture>) can follow its reader's own.
const PAGES = (s) => [
  { name: 'home', path: '/home', expect: (t) => ['local', 'node-1', 'node-2', 'docker-tls', 'Lab'].every((w) => t.includes(w)) && /Agent \d/.test(t) },
  { name: 'environments', path: '/environments', expect: (t) => t.includes('Machines to try things on') && t.includes('docker-tls') && /\bup\b/.test(t) },
  { name: 'containers', path: '/containers', host: s.local.id, expect: (t) => ['whoami-whoami-1', 'local-registry', 'nightly-job', 'healthy'].every((w) => t.includes(w)) && / MB/.test(t) },
  { name: 'stack', path: `/stacks/${s.stack.id}`, host: s.local.id, expect: (t) => t.includes('whoami-cache-1') && t.includes('whoami-whoami-1') && /2 of 2/.test(t) },
  { name: 'container', path: '/containers/db', host: s.local.id, expect: (t) => t.includes('postgres:16-alpine') && t.includes('healthy') },
];
const SHOTS = (s) => PAGES(s).flatMap((p) => [{ ...p, theme: 'light' }, { ...p, name: `${p.name}-dark`, theme: 'dark' }]);

// ---- a Chromium over the DevTools protocol
async function browser() {
  if (!BROWSER) throw new Error('no Chromium-based browser found: set QA_BROWSER');
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'freightdock-shots-browser-'));
  const proc = spawn(BROWSER, ['--headless=new', '--remote-debugging-port=0', `--user-data-dir=${dir}`, '--no-first-run', '--no-default-browser-check',
    '--disable-extensions', '--force-color-profile=srgb', '--hide-scrollbars', '--lang=en-US', 'about:blank'], { stdio: 'ignore' });
  const portFile = path.join(dir, 'DevToolsActivePort');
  await until('the browser to start', async () => fs.existsSync(portFile) && fs.readFileSync(portFile, 'utf8').includes('\n'), 30);
  const [port, wsPath] = fs.readFileSync(portFile, 'utf8').trim().split('\n');
  const ws = new WebSocket(`ws://127.0.0.1:${port}${wsPath}`);
  await new Promise((ok, no) => { ws.onopen = ok; ws.onerror = no; });
  let next = 0; const waiting = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    const w = msg.id && waiting.get(msg.id);
    if (!w) return;
    waiting.delete(msg.id);
    msg.error ? w.no(new Error(`${w.method}: ${msg.error.message}`)) : w.ok(msg.result);
  };
  const send = (method, params = {}, sessionId) => new Promise((ok, no) => {
    const id = ++next; waiting.set(id, { ok, no, method });
    ws.send(JSON.stringify({ id, method, params, sessionId }));
  });
  const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
  const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
  const page = (method, params) => send(method, params, sessionId);
  const evaluate = async (expression) => (await page('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true })).result.value;
  return {
    send, page, evaluate,
    close() { try { ws.close(); } catch { /* gone */ } proc.kill(); setTimeout(() => fs.rmSync(dir, { recursive: true, force: true }), 500); },
  };
}

async function shoot(b, shot, version) {
  const [width, height] = shot.size || [1440, 900];
  await b.page('Emulation.setDeviceMetricsOverride', { width, height, deviceScaleFactor: 1, mobile: false });
  // This browser's choices, as the UI keeps them: the host the pages are about, the theme, and the patch notes seen.
  await b.page('Page.navigate', { url: BASE + '/login' });
  await until('the page to load', () => b.evaluate('document.readyState === "complete"'), 30);
  await b.evaluate(`localStorage.clear();
    localStorage.setItem('freightdock.seenVersion', ${JSON.stringify(version)});
    ${shot.host ? `localStorage.setItem('freightdock.env', '${shot.host}');` : ''}
    localStorage.setItem('freightdock.theme', '${shot.theme}'); true`);
  const nav = await b.page('Page.navigate', { url: BASE + shot.path });
  if (nav.errorText) throw new Error(`${shot.name}: ${BASE + shot.path}: ${nav.errorText}`);
  let text = '';
  try {
    await until(`${shot.name} to show what it is about`, async () => {
      text = await b.evaluate('document.body ? document.body.innerText : ""');
      return shot.expect(text);
    }, 60);
  } catch (e) {
    throw new Error(`${e.message}; the page says:\n${text.slice(0, 1500)}`);
  }
  await wait(shot.settle ?? 1500);
  text = await b.evaluate('document.body.innerText');
  const bad = text.match(/\b(null|undefined|NaN)\b|Shipshow/i);
  if (bad) throw new Error(`${shot.name}: the page says "${bad[0]}"`);
  const broken = await b.evaluate('[...document.images].filter((i) => i.complete && i.naturalWidth === 0).map((i) => i.src)');
  if (broken.length) throw new Error(`${shot.name}: broken images ${broken.join(', ')}`);
  const { data } = await b.page('Page.captureScreenshot', { format: 'png' });
  fs.mkdirSync(OUT, { recursive: true });
  fs.writeFileSync(path.join(OUT, `${shot.name}.png`), Buffer.from(data, 'base64'));
  say(`${shot.name}.png`);
}

const state = await setUp();
const shots = SHOTS(state).filter((s) => !ONLY.length || ONLY.includes(s.name));
if (ONLY.length && shots.length !== ONLY.length) throw new Error(`SHOTS_ONLY names a picture there is not: ${ONLY.join(', ')}`);
// Stats arrive every few seconds, and Home's numbers with each host's summary: give the pages something to show.
await wait(8000);
const b = await browser();
try {
  const [name, value] = cookie.split('=');
  await b.send('Storage.setCookies', { cookies: [{ name, value, domain: '127.0.0.1', path: '/', httpOnly: true, sameSite: 'Lax' }] });
  for (const shot of shots) await shoot(b, shot, state.version);
} finally {
  b.close();
}
say(`${shots.length} pictures in ${OUT}`);
