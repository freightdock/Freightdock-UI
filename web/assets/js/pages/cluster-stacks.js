// The cluster's stacks ("Cluster" 4): each stack with its workloads, where each runs and how it is, asked of the manager
// again every few seconds, under a search and filters by state, worker and reach ("Cluster" 20). An administrator
// deploys a stack or changes one on the deployer page (cluster-deploy.js), starts, stops or restarts it, or removes it.
// Placement and the containers themselves are the manager's. Each stack says how each of its services is reached from
// outside the cluster ("Cluster" 17, 19): on a pool address (an exposure, never in the Compose file) or not at all
// (internal only), with the ports its Compose file asks for that open nothing; an administrator sets the exposures.

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
import { segmented } from '../../finui/components/segmented/segmented.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t, tOr } from '../i18n.js';
import { nodeName as shownName } from './cluster.js';
import { exposureEditor } from './cluster-deploy.js';

const EVERY_MS = 3000;
/** The nodes' shown names by id, from the last load. */
let names = new Map();
const TONES = { running: 'good', stopped: 'info', pending: 'warning', failed: 'critical', unplaced: 'critical', unknown: 'warning', fenced: 'critical' };
const sec = (ms) => Math.floor((ms || 0) / 1000);
/** The managers' own stack, the UI: changed with `freightdock-manager ui`, reached on its pool address like any stack. */
const SYSTEM = 'freightdock';
/** The search and filters, kept while Freightdock is open so a way back finds them as they were. */
const filter = { q: '', state: 'all', node: '', reach: 'all' };

/** Whether stack `s` is one the filters keep. The search matches the stack's, its services', images', workloads' and
 *  workers' names, its addresses and its exposures, ignoring case. */
export function kept(s, f, nameOf) {
  const w = s.workloads || [];
  const up = w.filter((x) => x.status === 'running').length;
  const state = s.stopped ? 'stopped' : up === w.length ? 'running' : 'attention';
  if (f.state !== 'all' && f.state !== state) return false;
  if (f.node && !w.some((x) => x.node_id === f.node)) return false;
  const pooled = (s.exposures || []).length > 0;
  if (f.reach === 'pool' && !pooled) return false;
  if (f.reach === 'internal' && pooled) return false;
  const q = f.q.trim().toLowerCase();
  if (!q) return true;
  const words = [s.name, s.subnet, ...Object.entries(s.services || {}).flatMap(([k, v]) => [k, v.image]),
    ...w.flatMap((x) => [x.id, x.address, x.node_id && nameOf(x.node_id), x.hostname]),
    ...(s.exposures || []).map((e) => `${e.address}:${e.port}`)];
  return words.some((x) => x && String(x).toLowerCase().includes(q));
}

function toolbar(nodes, redraw) {
  const search = h('input', { class: 'fui-field__input fd-stacks__search', type: 'search', value: filter.q, placeholder: t('pages.cluster_stacks.search'), 'aria-label': t('pages.cluster_stacks.search') });
  search.addEventListener('input', () => { filter.q = search.value; redraw(); });
  const node = h('select', { class: 'fui-field__input fd-stacks__node', 'aria-label': t('pages.cluster_stacks.filter_node') },
    h('option', { value: '' }, t('pages.cluster_stacks.all_nodes')), nodes.map((n) => h('option', { value: n.id, selected: n.id === filter.node }, shownName(n))));
  node.addEventListener('change', () => { filter.node = node.value; redraw(); });
  return h('div', { class: 'fd-stacks__bar' }, search,
    segmented({ label: t('pages.cluster_stacks.filter_state'), size: 'sm', value: filter.state, onChange: (v) => { filter.state = v; redraw(); },
      options: ['all', 'running', 'attention', 'stopped'].map((v) => ({ value: v, label: t(`pages.cluster_stacks.state_${v}`) })) }),
    segmented({ label: t('pages.cluster_stacks.filter_reach'), size: 'sm', value: filter.reach, onChange: (v) => { filter.reach = v; redraw(); },
      options: ['all', 'pool', 'internal'].map((v) => ({ value: v, label: t(`pages.cluster_stacks.reach_filter_${v}`) })) }),
    node);
}

export function clusterStacksPage({ el, signal }) {
  const head = h('div'), bar = h('div'), list = h('div', { class: 'stack' });
  el.append(head, bar, list);
  // The bar is drawn again only when the workers change, so typing in it is never cut off by a reload; it always
  // redraws the latest load's list.
  let barFor = '', redrawNow = () => {};
  const reload = loadInto(list, {
    signal,
    load: async (s) => {
      const c = await get('/api/cluster', { signal: s });
      if (!c.connected) return { c, stacks: [], recoveries: [], nodes: [] };
      const [stacks, recoveries, nodes] = await Promise.all([get('/api/cluster/stacks', { signal: s }), get('/api/cluster/recoveries', { signal: s }), get('/api/cluster/nodes', { signal: s }).catch(() => [])]);
      return { c, stacks, recoveries, nodes };
    },
    draw: ({ c, stacks, recoveries, nodes }) => {
      // Nodes by the name given them in Freightdock ("Cluster" 12), else their hostname.
      names = new Map((nodes || []).map((n) => [n.id, shownName(n)]));
      mount(head, pageHeader(t('pages.cluster_stacks.title'), t('pages.cluster_stacks.sub'),
        c.connected && isAdmin() ? button({ size: 'sm', variant: 'primary', href: '/cluster/stacks/new' }, t('pages.cluster_stacks.deploy')) : null));
      if (!c.connected) { mount(bar); barFor = ''; return emptyState(t('pages.cluster.not_connected'), t('pages.cluster.not_connected_text')); }
      if (!stacks.length) { mount(bar); barFor = ''; return emptyState(t('pages.cluster_stacks.none'), t('pages.cluster_stacks.none_text')); }
      const key = (nodes || []).map((n) => `${n.id}=${shownName(n)}`).join(',');
      if (key !== barFor) { barFor = key; mount(bar, toolbar(nodes || [], () => redrawNow())); }
      const shown = h('div', { class: 'stack' });
      redrawNow = () => {
        const keep = stacks.filter((s) => kept(s, filter, (id) => names.get(id)));
        mount(shown, h('p', { class: 'sub', 'aria-live': 'polite' }, t('pages.cluster_stacks.showing', { n: keep.length, of: stacks.length })),
          ...(keep.length ? keep.map((s) => stackCard(s, reload)) : [emptyState(t('pages.cluster_stacks.none_match'), t('pages.cluster_stacks.none_match_text'))]));
      };
      redrawNow();
      return h('div', { class: 'stack' }, shown, recoveryCard(recoveries));
    },
  });
  const timer = setInterval(() => { if (!document.hidden) reload(); }, EVERY_MS);
  signal.addEventListener('abort', () => clearInterval(timer));
}

function stackCard(s, reload) {
  const w = s.workloads || [];
  const up = w.filter((x) => x.status === 'running').length;
  const state = s.stopped ? status({ tone: 'info' }, t('pages.cluster_stacks.stopped')) : status({ tone: up === w.length ? 'good' : 'warning' }, t('pages.cluster_stacks.running_n', { n: up, of: w.length }));
  const actions = isAdmin() ? h('div', { class: 'row nowrap' },
    s.stopped
      ? button({ size: 'sm', onClick: () => act(s.name, 'start', reload) }, t('pages.cluster_stacks.start'))
      : button({ size: 'sm', onClick: () => act(s.name, 'stop', reload) }, t('pages.cluster_stacks.stop')),
    s.stopped ? null : button({ size: 'sm', onClick: () => act(s.name, 'restart', reload) }, t('pages.cluster_stacks.restart')),
    s.name === SYSTEM ? null : button({ size: 'sm', href: `/cluster/stacks/${encodeURIComponent(s.name)}/edit` }, t('pages.cluster_stacks.edit')),
    button({ size: 'sm', onClick: () => exposeDialog(s, reload) }, t('pages.cluster_stacks.exposures')),
    s.name === SYSTEM ? null : button({ size: 'sm', variant: 'danger', onClick: () => removeStack(s.name, reload) }, t('common.remove'))) : null;
  const services = Object.entries(s.services || {}).map(([k, v]) => `${k} (${v.image}, ${v.replicas}x)`).join(', ');
  return card({
    title: h('span', { class: 'row' }, h('span', { class: 'mono' }, s.name), state),
    sub: h('span', null, h('span', { title: when(sec(s.updated_at)) }, t('pages.cluster_stacks.updated', { when: ago(sec(s.updated_at)), services })),
      s.subnet ? h('span', { class: 'mono' }, ` · ${t('pages.cluster_stacks.network', { subnet: s.subnet })}`) : null,
      h('span', null, ` · ${(s.nodes || []).length ? t('pages.cluster_stacks.runs_on', { nodes: s.nodes.map((id) => names.get(id) || shortId(id)).join(', ') }) : t('pages.cluster_stacks.runs_anywhere')}`)),
    actions,
    body: h('div', { class: 'stack' }, workloadTable(w), reachTable(s), volumesLine(s)),
  });
}

/** Its volumes on the cluster's shared storage ("Cluster" 21), with the worker holding each one for one writer. */
function volumesLine(s) {
  const v = Object.entries(s.volumes || {});
  if (!v.length) return null;
  return h('p', { class: 'sub' }, t('pages.cluster_stacks.volumes'), ' ', v.map(([name, x], i) => h('span', { class: 'nowrap' }, i ? ', ' : '',
    h('span', { class: 'mono' }, name), ' ', t(x.access === 'many' ? 'pages.cluster_stacks.volume_many' : x.holder ? 'pages.cluster_stacks.volume_held' : 'pages.cluster_stacks.volume_one',
      { storage: x.storage, node: x.holder ? names.get(x.holder) || shortId(x.holder) : '' }))));
}

/** How each service is reached from outside the cluster: its exposures on the pool, or nothing (internal only); and
 *  the ports its Compose file asks for that no exposure answers, which open nothing ("Cluster" 19). */
export function reachOf(s) {
  return Object.entries(s.services || {}).map(([name, svc]) => {
    const mine = (s.exposures || []).filter((e) => e.service === name);
    const pool = mine.map((e) => ({ kind: 'pool', text: `${e.address}:${e.port} → ${e.container_port}/${e.protocol}` }));
    const asked = (svc.ports || []).filter((p) => p.host_port && !mine.some((e) => e.container_port === p.container_port && e.protocol === (p.protocol || 'tcp')))
      .map((p) => ({ kind: 'asked', text: `${p.host_port}:${p.container_port}/${p.protocol || 'tcp'}` }));
    return { name, ways: [...pool, ...asked] };
  });
}

function reachTable(s) {
  return table({
    rows: reachOf(s), filter: false, dense: true, label: t('pages.cluster_stacks.reach'),
    columns: [
      { label: t('pages.cluster_stacks.service'), cell: (r) => h('span', { class: 'mono' }, r.name), sort: (r) => r.name },
      { label: t('pages.cluster_stacks.reach'), cell: (r) => {
        const pool = r.ways.filter((w) => w.kind === 'pool'), asked = r.ways.filter((w) => w.kind === 'asked');
        return h('div', { class: 'row wrap' },
          pool.length ? pool.map((w) => h('span', { title: t('pages.cluster_stacks.reach_pool_help') }, status({ tone: 'good' }, t('pages.cluster_stacks.reach_pool', { what: w.text }))))
            : h('span', { class: 'sub', title: t('pages.cluster_stacks.reach_internal_help') }, t('pages.cluster_stacks.reach_internal')),
          asked.map((w) => h('span', { class: 'sub', title: t('pages.cluster_stacks.reach_asked_help') }, t('pages.cluster_stacks.reach_asked', { what: w.text }))));
      } },
    ],
  });
}

/** The stack's exposures on the pool, all of them, edited as rows (the deployer's editor); the managers check them
 *  whole. The ports its Compose file asks for that have none are offered, internal only until an address is chosen. */
async function exposeDialog(s, reload) {
  let pool;
  try { pool = await get('/api/cluster/floating'); } catch (e) { toast({ text: e.message, tone: 'critical' }); return; }
  const editor = exposureEditor({ pool, stack: s.name, initial: s.exposures || [] });
  editor.offer(s.services || {}, false);
  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, editor.el, err, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.cluster_stacks.exposures_of', { name: s.name }), body: form, wide: true });
  form.addEventListener('submit', async (ev) => {
    ev.preventDefault();
    const exposures = editor.read();
    if (!exposures) { err.textContent = t('pages.cluster_stacks.exposure_incomplete'); return; }
    go.disabled = true;
    try {
      await put(`/api/cluster/stacks/${encodeURIComponent(s.name)}/exposures`, { exposures });
      m.close();
      toast({ text: t('pages.cluster_stacks.exposures_saved', { name: s.name }), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; err.textContent = x.message; }
  });
}

/** An IPv4 address as a key that sorts by number; none last. */
export function addrKey(a) {
  return a ? a.split('.').map((x) => x.padStart(3, '0')).join('.') : '~';
}

function lastLine(c) {
  if (!c) return '';
  return `${tOr(`pages.cluster_stacks.op.${c.op && c.op.op}`, (c.op && c.op.op) || '')}: ${tOr(`pages.cluster_stacks.cmd.${c.status}`, c.status)}${c.message ? `, ${c.message}` : ''}`;
}

function workloadTable(rows) {
  return table({
    rows, filter: false, dense: true, empty: [t('pages.cluster_stacks.no_workloads'), ''],
    columns: [
      { label: t('pages.cluster_stacks.workload'), cell: (w) => h('span', { class: 'mono' }, w.id), sort: (w) => w.id },
      { label: t('pages.cluster.node'), cell: (w) => (w.node_id ? h('div', { class: 'name-cell' }, h('a', { href: `/cluster/nodes/${encodeURIComponent(w.node_id)}` }, names.get(w.node_id) || w.hostname || shortId(w.node_id)), h('span', { class: 'sub mono', title: w.node_id }, shortId(w.node_id))) : h('span', { class: 'sub' }, t('pages.cluster_stacks.no_node'))), sort: (w) => names.get(w.node_id) || w.hostname || '' },
      { label: t('pages.cluster_stacks.address'), cell: (w) => (w.address ? h('span', { class: 'mono' }, w.address) : h('span', { class: 'sub' }, t('pages.cluster_stacks.no_address'))), sort: (w) => addrKey(w.address) },
      { label: t('pages.cluster_stacks.status'), cell: (w) => status({ tone: TONES[w.status] || 'info', line: true }, tOr(`pages.cluster_stacks.status_word.${w.status}`, w.status)), sort: (w) => w.status },
      { label: t('pages.cluster_stacks.recovery'), cell: (w) => h('span', { class: 'sub' }, tOr(`pages.cluster_stacks.recovery_kind.${w.recovery}`, w.recovery || '')), sort: (w) => w.recovery || '' },
      { label: t('pages.cluster_stacks.container'), cell: (w) => (w.actual ? h('span', { class: 'sub' }, `${w.actual.state}${w.actual.state === 'exited' ? ` (${w.actual.exit_code})` : ''}`) : '') },
      { label: t('pages.cluster_stacks.last'), cell: (w) => h('span', { class: 'truncate sub', title: lastLine(w.last_command) }, lastLine(w.last_command)) },
    ],
  });
}

const nodeName = (host, id) => (id ? names.get(id) || `${host || ''} ${shortId(id)}`.trim() : '');

/** The managers' recovery history ("Cluster" 7), newest first, the latest 50. */
function recoveryCard(rows) {
  return card({
    title: t('pages.cluster_stacks.recoveries'),
    sub: t('pages.cluster_stacks.recoveries_sub'),
    body: table({
      rows: (rows || []).slice(0, 50), filter: false, dense: true, empty: [t('pages.cluster_stacks.recoveries_none'), ''],
      columns: [
        { label: t('pages.events.when'), cell: (r) => h('span', { title: when(sec(r.at)) }, ago(sec(r.at))), sort: (r) => -r.at },
        { label: t('pages.cluster_stacks.workload'), cell: (r) => h('span', { class: 'mono' }, r.workload_id), sort: (r) => r.workload_id },
        { label: t('pages.cluster_stacks.from'), cell: (r) => h('span', { class: 'sub mono' }, nodeName(r.from_hostname, r.from)) },
        { label: t('pages.events.detail'), cell: (r) => (r.to
          ? status({ tone: 'good', line: true }, t('pages.cluster_stacks.moved', { node: nodeName(r.to_hostname, r.to) }))
          : h('span', { class: 'truncate sub', title: r.reason }, t('pages.cluster_stacks.stays', { reason: r.reason }))) },
      ],
    }),
  });
}

async function act(name, action, reload) {
  try {
    const r = await post(`/api/cluster/stacks/${encodeURIComponent(name)}/${action}`, {});
    toast({ text: action === 'restart' ? t('pages.cluster_stacks.restarting', { name, n: r.restarting || 0 }) : t(action === 'start' ? 'pages.cluster_stacks.starting' : 'pages.cluster_stacks.stopping', { name }), tone: 'good' });
    reload();
  } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

async function removeStack(name, reload) {
  if (!(await confirm({ title: t('pages.cluster_stacks.remove_title', { name }), body: t('pages.cluster_stacks.remove_body') }))) return;
  try { await del(`/api/cluster/stacks/${encodeURIComponent(name)}`); toast({ text: t('pages.cluster_stacks.removed', { name }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}
