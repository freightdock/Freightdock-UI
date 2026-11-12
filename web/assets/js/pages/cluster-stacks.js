// The cluster's stacks ("Cluster" 4): each stack with its workloads, where each runs and how it is, asked of the manager
// again every few seconds. An administrator deploys a stack from a Compose file (or changes one by deploying it
// again), starts, stops or restarts it, or removes it. Placement and the containers themselves are the manager's.

import { h, mount, ago, when, shortId } from '../dom.js';
import { get, post, del } from '../api.js';
import { isAdmin } from '../state.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { codeEditor } from '../editor.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t, tOr } from '../i18n.js';

const EVERY_MS = 3000;
const TONES = { running: 'good', stopped: 'info', pending: 'warning', failed: 'critical', unplaced: 'critical', unknown: 'warning', fenced: 'critical' };
const sec = (ms) => Math.floor((ms || 0) / 1000);
// A Compose file to start from: Compose's own words.
const EXAMPLE = `services:
  web:
    image: nginx:alpine
    ports: ["8080:80"]
    deploy:
      replicas: 2
`;

export function clusterStacksPage({ el, signal }) {
  const head = h('div'), list = h('div', { class: 'stack' });
  el.append(head, list);
  const reload = loadInto(list, {
    signal,
    load: async (s) => {
      const c = await get('/api/cluster', { signal: s });
      if (!c.connected) return { c, stacks: [], recoveries: [] };
      const [stacks, recoveries] = await Promise.all([get('/api/cluster/stacks', { signal: s }), get('/api/cluster/recoveries', { signal: s })]);
      return { c, stacks, recoveries };
    },
    draw: ({ c, stacks, recoveries }) => {
      mount(head, pageHeader(t('pages.cluster_stacks.title'), t('pages.cluster_stacks.sub'),
        c.connected && isAdmin() ? button({ size: 'sm', variant: 'primary', onClick: () => deployDialog(null, reload) }, t('pages.cluster_stacks.deploy')) : null));
      if (!c.connected) return emptyState(t('pages.cluster.not_connected'), t('pages.cluster.not_connected_text'));
      if (!stacks.length) return emptyState(t('pages.cluster_stacks.none'), t('pages.cluster_stacks.none_text'));
      return h('div', { class: 'stack' }, ...stacks.map((s) => stackCard(s, reload)), recoveryCard(recoveries));
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
    button({ size: 'sm', onClick: () => deployDialog(s.name, reload) }, t('pages.cluster_stacks.edit')),
    button({ size: 'sm', variant: 'danger', onClick: () => removeStack(s.name, reload) }, t('common.remove'))) : null;
  const services = Object.entries(s.services || {}).map(([k, v]) => `${k} (${v.image}, ${v.replicas}x)`).join(', ');
  return card({
    title: h('span', { class: 'row' }, h('span', { class: 'mono' }, s.name), state),
    sub: h('span', { title: when(sec(s.updated_at)) }, t('pages.cluster_stacks.updated', { when: ago(sec(s.updated_at)), services })),
    actions,
    body: workloadTable(w),
  });
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
      { label: t('pages.cluster.node'), cell: (w) => (w.node_id ? h('div', { class: 'name-cell' }, h('span', null, w.hostname || shortId(w.node_id)), h('span', { class: 'sub mono', title: w.node_id }, shortId(w.node_id))) : h('span', { class: 'sub' }, t('pages.cluster_stacks.no_node'))) },
      { label: t('pages.cluster_stacks.status'), cell: (w) => status({ tone: TONES[w.status] || 'info', line: true }, tOr(`pages.cluster_stacks.status_word.${w.status}`, w.status)), sort: (w) => w.status },
      { label: t('pages.cluster_stacks.recovery'), cell: (w) => h('span', { class: 'sub' }, tOr(`pages.cluster_stacks.recovery_kind.${w.recovery}`, w.recovery || '')), sort: (w) => w.recovery || '' },
      { label: t('pages.cluster_stacks.container'), cell: (w) => (w.actual ? h('span', { class: 'sub' }, `${w.actual.state}${w.actual.state === 'exited' ? ` (${w.actual.exit_code})` : ''}`) : '') },
      { label: t('pages.cluster_stacks.last'), cell: (w) => h('span', { class: 'truncate sub', title: lastLine(w.last_command) }, lastLine(w.last_command)) },
    ],
  });
}

const nodeName = (host, id) => (id ? `${host || ''} ${shortId(id)}`.trim() : '');

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

/** Deploys a new stack (`existing` null) or a changed one, from its Compose file. */
async function deployDialog(existing, reload) {
  let compose = EXAMPLE;
  if (existing) {
    try { compose = (await get(`/api/cluster/stacks/${encodeURIComponent(existing)}`)).compose || ''; } catch (e) { toast({ text: e.message, tone: 'critical' }); return; }
  }
  const name = formField({ id: 'cs-name', label: t('pages.cluster_stacks.name'), autocomplete: 'off', help: t('pages.cluster_stacks.name_help') });
  name.input.value = existing || '';
  if (existing) name.input.readOnly = true;
  const file = codeEditor({ id: 'cs-compose', label: t('pages.cluster_stacks.compose'), value: compose, rows: 18, language: 'yaml', help: t('pages.cluster_stacks.compose_help') });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.cluster_stacks.deploy'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, file.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: existing ? t('pages.cluster_stacks.edit_title', { name: existing }) : t('pages.cluster_stacks.deploy'), body: form, wide: true, initialFocus: existing ? undefined : name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const n = name.input.value.trim();
    if (!/^[a-z0-9][a-z0-9-]{0,39}$/.test(n)) { name.setError(t('pages.cluster_stacks.name_help')); return; }
    go.disabled = true;
    try {
      const r = await post('/api/cluster/stacks', { name: n, compose: file.value });
      m.close();
      toast({ text: t('pages.cluster_stacks.deployed', { name: n }), tone: 'good' });
      for (const w of r.warnings || []) toast({ text: w, tone: 'warning' });
      reload();
    } catch (err) {
      go.disabled = false;
      toast({ text: err.message, tone: 'critical' });
    }
  });
}
