// The cluster ("Cluster" 1): its managers (which leads, and on which node), its workers (every node) with their health
// as the managers agree on it and which manager hears each, each opening its own page, and the latest events, all asked of the manager again every few seconds. An administrator
// sends a node a command (a ping, or a container run, stopped or removed), removes a node, or makes a join token to add
// one ("Cluster" 2). Under the workers the floating addresses, each with the worker holding it; an administrator adds
// or removes one.

import { h, mount, ago, when, shortId } from '../dom.js';
import { get, post, put, del } from '../api.js';
import { isAdmin } from '../state.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { codeLine } from '../../finui/components/code-block/code-block.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t, tOr } from '../i18n.js';

const EVERY_MS = 3000;
const HEALTH_TONES = { healthy: 'good', suspect: 'warning', unreachable: 'critical' };
const FENCE_TONES = { requested: 'warning', in_progress: 'warning', fenced: 'critical', failed: 'critical', awaiting_authorization: 'warning', authorized: 'info' };
const sec = (ms) => Math.floor((ms || 0) / 1000);

export function theClusterPage({ el, signal }) {
  const head = h('div');
  const managers = h('div'), nodes = h('div'), floats = h('div'), events = h('div');
  el.append(head, managers, nodes, floats, events);

  const reload = loadInto(managers, {
    signal,
    load: async (s) => {
      const [c, n, e, f] = await Promise.all([get('/api/cluster', { signal: s }), get('/api/cluster/nodes', { signal: s }).catch(() => null), get('/api/cluster/events', { signal: s }).catch(() => null), get('/api/cluster/floating', { signal: s }).catch(() => null)]);
      return { c, n, e, f };
    },
    draw: ({ c, n, e, f }) => {
      if (!c.connected) {
        mount(head, pageHeader(t('nav.the_cluster'), t('pages.cluster.sub')));
        mount(nodes); mount(floats); mount(events);
        return emptyState(t('pages.cluster.not_connected'), t('pages.cluster.not_connected_text'));
      }
      const cl = c.cluster || {};
      mount(head, pageHeader(t('nav.the_cluster'), t('pages.cluster.id', { id: cl.cluster_id || '' }), isAdmin() ? h('div', { class: 'row' },
        button({ size: 'sm', onClick: () => joinToken('worker') }, t('pages.cluster.add_node')),
        button({ size: 'sm', onClick: () => joinToken('manager') }, t('pages.cluster.add_manager'))) : null));
      mount(nodes, card({ title: t('pages.cluster.workers'), body: n ? nodeTable(n, cl.raft || [], reload) : emptyState(t('pages.cluster.no_answer'), '') }));
      mount(floats, card({ title: t('pages.cluster.floating'), sub: t('pages.cluster.floating_sub'),
        actions: isAdmin() ? button({ size: 'sm', onClick: () => floatingDialog(reload) }, t('pages.cluster.floating_add')) : null,
        body: f ? floatingTable(f, n || [], reload) : emptyState(t('pages.cluster.no_answer'), '') }));
      mount(events, card({ title: t('pages.cluster.events'), body: e ? eventTable(e) : emptyState(t('pages.cluster.no_answer'), '') }));
      return card({ title: t('pages.cluster.managers'), body: managerTable(cl.raft || [], n || []) });
    },
  });
  const timer = setInterval(() => { if (!document.hidden) reload(); }, EVERY_MS);
  signal.addEventListener('abort', () => clearInterval(timer));
}

/** The host of `host:port` (`[v6]:port` too). */
export function hostOf(addr) {
  if (!addr) return '';
  const i = addr.lastIndexOf(':');
  return (i > 0 ? addr.slice(0, i) : addr).replace(/^\[|\]$/g, '');
}

/** What a node is shown by: the name given it in Freightdock, else its hostname, else its id. */
export function nodeName(n) {
  return n.name || n.hostname || shortId(n.id);
}

/** The node's address as the managers see it: where its worker connects from, or for one connected over loopback the
 *  manager that hears it. */
export function nodeHost(n) {
  const l = n.liveness || {};
  const h0 = hostOf(l.peer);
  if (h0 === '127.0.0.1' || h0 === '::1' || h0 === 'localhost') return l.via ? hostOf(l.via) : '';
  return h0;
}

/** What a manager is shown by: the name given it in Freightdock ("Cluster" 14), else its host. */
export function managerName(m) {
  return m.name || hostOf(m.addr);
}

/** The manager a node's worker is heard by: the one named in `via`, else (heard, no `via`) the one answering. */
export function heardBy(n, raft) {
  const l = n.liveness || {};
  if (l.last_heard_ms_ago == null && !l.via) return null;
  return l.via ? raft.find((m) => hostOf(m.addr) === hostOf(l.via)) || { addr: l.via } : raft.find((m) => m.me) || null;
}

/** A link to the manager's page, or its address when it is not a member known. */
export function managerLink(m) {
  if (!m) return '';
  return m.raft_id ? h('a', { href: `/cluster/managers/${encodeURIComponent(m.raft_id)}` }, managerName(m)) : h('span', { class: 'mono sub' }, m.addr);
}

/** The managers' hosts, to tell which nodes run a manager too. */
export function managerHosts(raft) {
  return new Set(raft.map((m) => hostOf(m.addr)));
}

function managerTable(raft, list) {
  const onNode = (m) => list.find((n) => nodeHost(n) === hostOf(m.addr));
  return table({
    rows: raft, filter: false, dense: true, empty: [t('pages.cluster.no_managers'), ''],
    columns: [
      { label: t('pages.cluster.manager'), cell: (m) => h('div', { class: 'name-cell' }, managerLink(m), m.name ? h('span', { class: 'sub mono' }, m.addr) : null), sort: (m) => managerName(m) },
      { label: t('pages.cluster.online'), cell: (m) => status({ tone: m.online ? 'good' : 'critical', line: true }, m.online ? t('pages.cluster.online_yes') : t('pages.cluster.online_no')) },
      { label: t('pages.cluster.node'), cell: (m) => { const n = onNode(m); return n ? h('a', { href: `/cluster/nodes/${encodeURIComponent(n.id)}` }, nodeName(n)) : h('span', { class: 'sub' }, t('pages.cluster.no_worker')); } },
      { label: t('pages.cluster.role'), cell: (m) => status({ tone: m.leader ? 'good' : m.voter ? 'info' : 'warning' }, m.leader ? t('pages.cluster.leader') : m.voter ? t('pages.cluster.voter') : t('pages.cluster.learner')) },
      { label: t('pages.cluster.raft_id'), cell: (m) => h('span', { class: 'mono sub' }, String(m.raft_id)) },
      { label: '', cell: (m) => (m.me ? h('span', { class: 'sub' }, t('pages.cluster.this_one')) : '') },
    ],
  });
}

function labels(n) {
  const l = n.all_labels || (n.report && n.report.labels) || {};
  return Object.entries(l).map(([k, v]) => `${k}=${v}`).join(', ');
}

function nodeTable(list, raft, reload) {
  const hosts = managerHosts(raft);
  return table({
    rows: list, empty: [t('pages.cluster.no_nodes'), t('pages.cluster.no_nodes_text')], label: t('pages.cluster.nodes'),
    columns: [
      { label: t('pages.cluster.node'), cell: (n) => h('div', { class: 'name-cell' }, h('a', { href: `/cluster/nodes/${encodeURIComponent(n.id)}` }, nodeName(n)), h('span', { class: 'sub mono', title: n.id }, n.name ? `${n.hostname || ''} · ${shortId(n.id)}` : shortId(n.id))), sort: (n) => nodeName(n) },
      { label: t('pages.cluster.role'), cell: (n) => hosts.has(nodeHost(n)) ? status({ tone: 'info' }, t('pages.cluster.role_manager')) : h('span', { class: 'sub' }, t('pages.cluster.role_worker')), sort: (n) => (hosts.has(nodeHost(n)) ? 0 : 1) },
      { label: t('pages.cluster.health'), cell: (n) => { const hl = (n.liveness || {}).health || 'unreachable'; return status({ tone: HEALTH_TONES[hl] || 'info', line: true }, tOr(`pages.cluster.health_word.${hl}`, hl)); }, sort: (n) => (n.liveness || {}).health || '' },
      { label: t('pages.cluster.heard'), cell: (n) => { const ms = (n.liveness || {}).last_heard_ms_ago; return ms == null ? '' : h('span', { class: 'nowrap' }, t('pages.cluster.seconds', { n: (ms / 1000).toFixed(1) })); } },
      { label: t('pages.cluster.via'), cell: (n) => managerLink(heardBy(n, raft)) },
      { label: t('pages.cluster.fencing'), cell: (n) => { const f = n.fencing || {}; const st = f.state || 'none'; return st === 'none' ? h('span', { class: 'sub' }, t('pages.cluster.fence.none')) : h('span', { title: f.message || '' }, status({ tone: FENCE_TONES[st] || 'info' }, tOr(`pages.cluster.fence.${st}`, st))); }, sort: (n) => (n.fencing || {}).state || '' },
      { label: t('pages.cluster.docker'), cell: (n) => (n.report && n.report.docker_version) || '' },
      { label: t('pages.cluster.labels'), cell: (n) => h('span', { class: 'truncate sub' }, labels(n)) },
      ...(isAdmin() ? [{ label: '', nosort: true, cell: (n) => h('div', { class: 'row row--end nowrap' },
        (n.fencing || {}).state === 'awaiting_authorization' ? button({ size: 'sm', variant: 'primary', onClick: () => authorize(n, reload) }, t('pages.cluster.authorize')) : null,
        button({ size: 'sm', onClick: () => send(n, { op: 'ping' }) }, t('pages.cluster.ping')),
        button({ size: 'sm', onClick: () => runDialog(n) }, t('pages.cluster.run')),
        button({ size: 'sm', onClick: () => workloadDialog(n) }, t('pages.cluster.workload')),
        button({ size: 'sm', onClick: () => labelsDialog(n, reload) }, t('pages.cluster.labels')),
        button({ size: 'sm', variant: 'danger', onClick: () => removeNode(n, reload) }, t('common.remove'))) }] : []),
    ],
  });
}

const EVENT_TONES = { connected: 'good', enrolled: 'good', disconnected: 'warning', removed: 'critical', command_expired: 'warning', health: 'info', manager_joined: 'good', recovery: 'warning', floating: 'info' };
function eventTable(list) {
  const rows = list.slice(-50).reverse();
  return table({
    rows, filter: false, dense: true, empty: [t('pages.cluster.no_events'), ''],
    columns: [
      { label: t('pages.events.when'), cell: (e) => h('span', { class: 'nowrap', title: when(sec(e.at)) }, ago(sec(e.at))), sort: (e) => e.seq },
      { label: t('pages.events.what'), cell: (e) => status({ tone: EVENT_TONES[e.kind] || 'info' }, tOr(`pages.cluster.event.${e.kind}`, e.kind)) },
      { label: t('pages.cluster.node'), cell: (e) => h('span', { class: 'mono sub' }, e.node_id ? shortId(e.node_id) : '') },
      { label: t('pages.events.detail'), cell: (e) => h('span', { class: 'truncate', title: e.message || '' }, e.message || '') },
    ],
  });
}

/** Sends a command and follows it until it ends (or a minute passes), saying how it went. */
async function send(n, body) {
  const name = nodeName(n);
  try {
    const c = await post(`/api/cluster/nodes/${encodeURIComponent(n.id)}/commands`, body);
    toast({ text: t('pages.cluster.sent', { op: body.op, name }), tone: 'info' });
    const until = Date.now() + 60000;
    while (Date.now() < until) {
      await new Promise((r) => setTimeout(r, 500));
      const s = await get(`/api/cluster/commands/${encodeURIComponent(c.id)}`);
      if (['succeeded', 'failed', 'rejected', 'expired'].includes(s.status)) {
        const ok = s.status === 'succeeded';
        toast({ text: t(ok ? 'pages.cluster.done' : 'pages.cluster.failed', { op: body.op, name, why: s.message || s.status }), tone: ok ? 'good' : 'critical' });
        return;
      }
    }
    toast({ text: t('pages.cluster.still_running', { op: body.op, name }), tone: 'warning' });
  } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

function runDialog(n) {
  const id = formField({ id: 'cl-workload', label: t('pages.cluster.workload_id'), autocomplete: 'off', help: t('pages.cluster.workload_help') });
  const image = formField({ id: 'cl-image', label: t('pages.cluster.image'), autocomplete: 'off', placeholder: 'nginx:alpine' });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.cluster.run'));
  const form = h('form', { class: 'stack', novalidate: true }, id.el, image.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.cluster.run_on', { name: nodeName(n) }), body: form, initialFocus: id.input });
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    if (!id.input.value.trim()) { id.setError(t('pages.cluster.needed')); return; }
    if (!image.input.value.trim()) { image.setError(t('pages.cluster.needed')); return; }
    m.close();
    send(n, { op: 'run_container', workload_id: id.input.value.trim(), image: image.input.value.trim() });
  });
}

function workloadDialog(n) {
  const id = formField({ id: 'cl-workload2', label: t('pages.cluster.workload_id'), autocomplete: 'off' });
  const act = (op) => () => {
    if (!id.input.value.trim()) { id.setError(t('pages.cluster.needed')); return; }
    m.close();
    send(n, { op, workload_id: id.input.value.trim() });
  };
  const m = openModal({
    title: t('pages.cluster.workload_on', { name: nodeName(n) }), initialFocus: id.input,
    body: h('div', { class: 'stack' }, id.el, h('div', { class: 'row row--end' },
      button({ onClick: act('stop_container') }, t('pages.cluster.stop')),
      button({ variant: 'danger', onClick: act('remove_container') }, t('common.remove')))),
  });
}

/** The node's labels as the cluster sets them, one key=value a line; the worker's own are shown, and these go over them. */
function labelsDialog(n, reload) {
  const own = Object.entries((n.report && n.report.labels) || {}).map(([k, v]) => `${k}=${v}`).join(', ');
  const area = h('textarea', { class: 'fui-field__input fui-field__input--area mono', id: 'cl-labels', rows: 6, spellcheck: false },
    Object.entries(n.labels || {}).map(([k, v]) => `${k}=${v}`).join('\n'));
  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true },
    h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: 'cl-labels' }, t('pages.cluster.labels')), area,
      h('p', { class: 'fui-field__help' }, t('pages.cluster.labels_help')),
      own ? h('p', { class: 'fui-field__help' }, t('pages.cluster.labels_own', { labels: own })) : null, err),
    h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.cluster.labels_on', { name: nodeName(n) }), body: form, initialFocus: area });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const labels = {};
    for (const line of area.value.split('\n').map((l) => l.trim()).filter(Boolean)) {
      const i = line.indexOf('=');
      if (i < 1) { err.textContent = t('pages.cluster.labels_bad', { line }); return; }
      labels[line.slice(0, i).trim()] = line.slice(i + 1).trim();
    }
    go.disabled = true;
    try {
      await put(`/api/cluster/nodes/${encodeURIComponent(n.id)}/labels`, { labels });
      m.close();
      toast({ text: t('pages.cluster.labels_saved', { name: nodeName(n) }), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; err.textContent = x.message; }
  });
}

/** Lets a node that came back fenced run workloads again; its workloads start at the next reconcile. */
async function authorize(n, reload) {
  const name = nodeName(n);
  if (!(await confirm({ title: t('pages.cluster.authorize_title', { name }), body: t('pages.cluster.authorize_body'), action: t('pages.cluster.authorize'), danger: false }))) return;
  try { await post(`/api/cluster/nodes/${encodeURIComponent(n.id)}/authorize`, {}); toast({ text: t('pages.cluster.authorized', { name }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

async function removeNode(n, reload) {
  const name = nodeName(n);
  if (!(await confirm({ title: t('pages.cluster.remove_title', { name }), body: t('pages.cluster.remove_body') }))) return;
  try { await del(`/api/cluster/nodes/${encodeURIComponent(n.id)}`); toast({ text: t('pages.cluster.removed', { name }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** The floating addresses with their holders by name. */
function floatingTable(list, nodes, reload) {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const holder = (x) => {
    if (!x.holder) return status({ tone: 'warning' }, t('pages.cluster.floating_waiting'));
    const n = byId.get(x.holder);
    return h('a', { href: `/cluster/nodes/${encodeURIComponent(x.holder)}` }, n ? nodeName(n) : shortId(x.holder));
  };
  return table({
    rows: list, empty: [t('pages.cluster.floating_none'), t('pages.cluster.floating_none_text')], label: t('pages.cluster.floating'),
    columns: [
      { label: t('pages.cluster.floating_address'), cell: (x) => h('span', { class: 'mono' }, x.address), sort: (x) => x.address.split('.').map((p) => p.padStart(3, '0')).join('.') },
      { label: t('pages.cluster.floating_holder'), cell: holder, sort: (x) => (x.holder && byId.get(x.holder) ? nodeName(byId.get(x.holder)) : '') },
      { label: t('pages.cluster.floating_exposes'), cell: (x) => ((x.exposures || []).length
        ? h('div', { class: 'row wrap' }, x.exposures.map((e) => h('span', { class: 'mono nowrap' }, `${e.port}/${e.protocol} → ${e.stack}.${e.service}:${e.container_port}`)))
        : h('span', { class: 'sub' }, t('pages.cluster.floating_exposes_nothing'))) },
      { label: t('pages.cluster.floating_since'), cell: (x) => (x.since ? h('span', { class: 'nowrap', title: when(sec(x.since)) }, ago(sec(x.since))) : '') },
      ...(isAdmin() ? [{ label: '', nosort: true, cell: (x) => h('div', { class: 'row row--end' }, button({ size: 'sm', variant: 'danger', onClick: () => removeFloating(x.address, reload) }, t('common.remove'))) }] : []),
    ],
  });
}

function floatingDialog(reload) {
  const f = formField({ id: 'cl-floating', label: t('pages.cluster.floating_address'), placeholder: '192.0.2.10', inputMode: 'decimal', help: t('pages.cluster.floating_add_help') });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.cluster.floating_add'));
  const form = h('form', { class: 'stack', novalidate: true }, f.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.cluster.floating_add'), body: form, initialFocus: f.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const address = f.input.value.trim();
    go.disabled = true;
    try {
      await post('/api/cluster/floating', { address });
      m.close();
      toast({ text: t('pages.cluster.floating_added', { address }), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; f.setError(x.message); }
  });
}

async function removeFloating(address, reload) {
  if (!(await confirm({ title: t('pages.cluster.floating_remove_title', { address }), body: t('pages.cluster.floating_remove_body') }))) return;
  try { await del(`/api/cluster/floating/${encodeURIComponent(address)}`); toast({ text: t('pages.cluster.floating_removed', { address }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** A join token, shown once with what to set on the new node. */
async function joinToken(kind) {
  let r;
  try { r = await post('/api/cluster/join-tokens', { kind }); } catch (e) { toast({ text: e.message, tone: 'critical' }); return; }
  const env = kind === 'manager' ? `FREIGHTDOCK_MANAGER_JOIN_TOKEN=${r.token}` : `FREIGHTDOCK_JOIN_TOKEN=${r.token}`;
  openModal({
    title: kind === 'manager' ? t('pages.cluster.add_manager') : t('pages.cluster.add_node'),
    body: h('div', { class: 'stack' }, h('p', null, t(kind === 'manager' ? 'pages.cluster.manager_token_text' : 'pages.cluster.worker_token_text')), codeLine(env),
      h('p', { class: 'sub' }, t('pages.cluster.token_once'))),
  });
}
