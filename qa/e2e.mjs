// The end-to-end suite: Freightdock against a real cluster (a manager and a worker), through the UI's API as the
// browser uses it. qa/e2e.sh starts the throwaway cluster (qa/cluster.sh) and runs this against it.
//
//   node qa/e2e.mjs <base> <setup-token-file>
//
// Each step prints `ok` or `FAIL` with what it saw; the exit status is the number of failures. Names it makes start
// with `e2e`, and it takes them away again, so it can run twice against one cluster.
import { readFileSync } from 'node:fs';
import { randomBytes } from 'node:crypto';

const [base, setupFile] = process.argv.slice(2);
if (!base || !setupFile) {
  console.error('usage: node qa/e2e.mjs <base> <setup-token-file>');
  process.exit(2);
}
// The throwaway cluster's UI has a certificate of its own making, on 127.0.0.1 only.
process.env.NODE_TLS_REJECT_UNAUTHORIZED = '0';

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
let failures = 0;
const ok = (what, cond, seen) => {
  console.log(`${cond ? 'ok  ' : 'FAIL'} ${what}${cond ? '' : `: ${typeof seen === 'string' ? seen : JSON.stringify(seen)}`}`);
  if (!cond) failures++;
  return cond;
};

/// A signed-in browser: its cookie and CSRF token.
class Session {
  cookie = '';
  csrf = '';
  async call(method, path, body) {
    const headers = { origin: base, cookie: this.cookie };
    if (body !== undefined) headers['content-type'] = 'application/json';
    if (method !== 'GET') headers['x-csrf-token'] = this.csrf;
    const r = await fetch(base + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
    const set = r.headers.getSetCookie?.() ?? [];
    if (set.length) this.cookie = set.map((c) => c.split(';')[0]).join('; ');
    const text = await r.text();
    let json = null;
    try { json = text ? JSON.parse(text) : null; } catch { json = text; }
    return { status: r.status, body: json };
  }
  async signIn(name, password) {
    const r = await this.call('POST', '/api/login', { name, password });
    if (r.status !== 200) return r;
    this.csrf = (await this.call('GET', '/api/me')).body.csrf;
    return r;
  }
}

/// Asks again until `test` holds or the time is up; the last answer either way.
async function until(seconds, ask, test) {
  let last;
  for (let i = 0; i < seconds; i++) {
    last = await ask();
    if (test(last)) return last;
    await sleep(1000);
  }
  return last;
}

const password = () => randomBytes(18).toString('base64url');
const admin = new Session();
const adminPw = password();

// --- the first run and signing in
{
  const token = readFileSync(setupFile, 'utf8').trim();
  let r = await admin.call('POST', '/api/setup', { name: 'e2e-admin', password: adminPw, token: 'not-the-token-at-all' });
  ok('setup refuses a wrong setup token', r.status >= 400 && r.status < 500, r);
  r = await admin.call('POST', '/api/setup', { name: 'e2e-admin', password: adminPw, token });
  if (r.status === 409 || (r.status >= 400 && /already|exists/i.test(JSON.stringify(r.body)))) {
    console.error('this cluster was set up before: start a fresh one (qa/cluster.sh down, then up)');
    process.exit(2);
  }
  ok('setup makes the first administrator', r.status < 300, r);
  r = await new Session().call('POST', '/api/login', { name: 'e2e-admin', password: 'wrong password here' });
  ok('a wrong password is refused', r.status === 401, r);
  r = await admin.signIn('e2e-admin', adminPw);
  ok('the administrator signs in', r.status === 200 && admin.csrf, r);
  r = await new Session().call('GET', '/api/cluster/nodes');
  ok('the cluster is not read without a session', r.status === 401, r);
}

// --- the cluster
const nodes = await until(60, () => admin.call('GET', '/api/cluster/nodes'), (r) => r.body?.[0]?.liveness?.health === 'healthy');
ok('the node is heard and healthy', nodes.status === 200 && nodes.body.length === 1 && nodes.body[0].liveness.health === 'healthy', nodes.body);
const node = nodes.body[0].id;
{
  const r = await admin.call('PUT', `/api/cluster/nodes/${node}/name`, { name: 'e2e-node' });
  ok('the node is renamed', r.status === 200 && r.body.name === 'e2e-node', r);
  const n = await admin.call('GET', `/api/cluster/nodes/${node}`);
  ok('the managers call it by that name too, its other labels kept', n.body.labels?.name === 'e2e-node' && n.body.all_labels?.zone === 'a', n.body.all_labels);
}

// --- stacks
const compose = 'services:\n  web:\n    image: nginx:alpine\n';
const workloads = (name) => admin.call('GET', `/api/cluster/stacks/${name}`).then((r) => r.body?.workloads ?? []);
const running = (ws) => ws.length > 0 && ws.every((w) => w.status === 'running');
{
  let r = await admin.call('POST', '/api/cluster/stack-check', { name: 'e2e-web', compose: 'services: [' });
  ok('a broken Compose file is refused by the check', r.status >= 400, r);
  r = await admin.call('POST', '/api/cluster/stacks', { name: 'e2e-web', compose });
  ok('a stack is deployed', r.status < 300, r);
  let ws = await until(90, () => workloads('e2e-web'), running);
  ok('its workload runs', running(ws), ws);
  const n = await admin.call('GET', `/api/cluster/nodes/${node}`);
  ok('the node reports its container', JSON.stringify(n.body.containers ?? []).includes('e2e-web'), n.body.containers);

  r = await admin.call('POST', '/api/cluster/stacks/e2e-web/stop', {});
  ok('the stack is stopped', r.status < 300, r);
  ws = await until(60, () => workloads('e2e-web'), (ws) => ws.every((w) => w.status !== 'running'));
  ok('its workload no longer runs', ws.every((w) => w.status !== 'running'), ws);
  r = await admin.call('POST', '/api/cluster/stacks/e2e-web/start', {});
  ok('the stack is started again', r.status < 300, r);
  ws = await until(90, () => workloads('e2e-web'), running);
  ok('its workload runs again', running(ws), ws);

  r = await admin.call('POST', '/api/cluster/stacks', { name: 'e2e-cold', compose, stopped: true });
  ok('a stack can be made stopped', r.status < 300, r);
  await sleep(5000);
  ws = await workloads('e2e-cold');
  ok('and nothing of it runs', ws.every((w) => w.status !== 'running'), ws);
}

// --- a plain user reads but does not change
{
  const userPw = password();
  let r = await admin.call('POST', '/api/users', { name: 'e2e-reader', password: userPw, role: 'user' });
  ok('an administrator makes a user', r.status < 300, r);
  const reader = new Session();
  await reader.signIn('e2e-reader', userPw);
  r = await reader.call('GET', '/api/cluster/stacks');
  ok('a user reads the stacks', r.status === 200 && JSON.stringify(r.body).includes('e2e-web'), r.status);
  r = await reader.call('POST', '/api/cluster/stacks', { name: 'e2e-nope', compose });
  ok('a user cannot deploy', r.status === 403, r);
  r = await reader.call('DELETE', '/api/cluster/stacks/e2e-web');
  ok('a user cannot remove a stack', r.status === 403, r);
  const id = (await admin.call('GET', '/api/users')).body.find?.((u) => u.name === 'e2e-reader')?.id;
  r = await admin.call('DELETE', `/api/users/${id}`);
  ok('the user is removed', r.status < 300, r);
}

// --- the MCP server, with an API token
{
  let r = await admin.call('POST', '/api/tokens', { name: 'e2e-mcp', expires_days: 1, read_only: true });
  ok('an API token is made', r.status < 300 && r.body.token, r.status);
  const { id, token } = r.body;
  const mcp = (msg) => fetch(base + '/mcp', { method: 'POST', headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json', accept: 'application/json, text/event-stream' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, ...msg }) }).then(async (x) => ({ status: x.status, body: await x.json().catch(() => null) }));
  r = await mcp({ method: 'initialize', params: { protocolVersion: '2025-06-18' } });
  ok('MCP initializes', r.status === 200 && r.body?.result?.serverInfo, r);
  r = await mcp({ method: 'tools/call', params: { name: 'list_stacks', arguments: {} } });
  ok('MCP lists the stacks', r.status === 200 && JSON.stringify(r.body).includes('e2e-web'), r);
  r = await admin.call('DELETE', `/api/tokens/${id}`);
  ok('the token is revoked', r.status < 300, r);
  r = await mcp({ method: 'tools/list' });
  ok('and refused after', r.status === 401, r.status);
}

// --- taking it away, on record
{
  for (const s of ['e2e-web', 'e2e-cold']) {
    const r = await admin.call('DELETE', `/api/cluster/stacks/${s}`);
    ok(`stack ${s} is removed`, r.status < 300, r);
  }
  const n = await until(60, () => admin.call('GET', `/api/cluster/nodes/${node}`), (n) => !JSON.stringify(n.body.containers ?? []).includes('e2e-'));
  ok('its containers are gone from the node', !JSON.stringify(n.body.containers ?? []).includes('e2e-'), n.body.containers);
  const audit = JSON.stringify((await admin.call('GET', '/api/audit?limit=200')).body);
  for (const a of ['cluster.node.rename', 'cluster.stack.deploy', 'cluster.stack.stop', 'cluster.stack.remove']) {
    ok(`the activity log has ${a}`, audit.includes(a), '');
  }
  await admin.call('PUT', `/api/cluster/nodes/${node}/name`, { name: '' });
}

console.log(failures ? `${failures} failed` : 'all passed');
process.exit(Math.min(failures, 100));
