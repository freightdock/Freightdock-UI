// One node of the cluster: what the managers know of it, the stacks' workloads placed on it and the containers it
// says it runs, asked again every few seconds. An administrator gives it a name to be shown by, in Freightdock only
// ("Cluster" 12): the managers and the node keep its hostname. An administrator opens its Docker from here ("Cluster" 25), and a shell on it or in a
// container it runs ("Cluster" 35).

import { h, mount, bytes, shortId } from '../dom.js';
import { get, put } from '../api.js';
import { isAdmin } from '../state.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t, tOr } from '../i18n.js';
import { nodeName, nodeHost, managerHosts, heardBy, managerLink } from './cluster.js';
import { addrKey } from './cluster-stacks.js';

const EVERY_MS = 3000;
const HEALTH_TONES = { healthy: 'good', suspect: 'warning', unreachable: 'critical' };
const STATE_TONES = { running: 'good', restarting: 'warning', paused: 'warning', exited: 'critical', dead: 'critical', created: 'info' };
const WORKLOAD_TONES = { running: 'good', stopped: 'info', pending: 'warning', failed: 'critical', unplaced: 'critical', unknown: 'warning', fenced: 'critical' };

export function clusterNodePage({ el, params, signal }) {
  const head = h('div'), body = h('div');
  el.append(head, body);
  const id = params.id;
  const reload = loadInto(body, {
    signal,
    load: async (s) => {
      const [n, c, st] = await Promise.all([
        get(`/api/cluster/nodes/${encodeURIComponent(id)}`, { signal: s }),
        get('/api/cluster', { signal: s }).catch(() => ({})),
        get('/api/cluster/stacks', { signal: s }).catch(() => []),
      ]);
      return { n, c, st };
    },
    draw: ({ n, c, st }) => {
      const raft = ((c || {}).cluster || {}).raft || [];
      const manager = managerHosts(raft).has(nodeHost(n));
      mount(head, pageHeader(nodeName(n), n.name ? t('pages.cluster_node.sub_named', { hostname: n.hostname || '' }) : t('pages.cluster_node.sub'),
        h('div', { class: 'row' },
          button({ size: 'sm', href: '/cluster' }, t('pages.cluster_node.back')),
          isAdmin() && (n.report || {}).docker ? button({ size: 'sm', href: `/cluster/nodes/${encodeURIComponent(n.id)}/docker` }, t('pages.cluster_node.docker')) : null,
          isAdmin() && (n.report || {}).shells && n.report.host_shell ? button({ size: 'sm', href: `/cluster/nodes/${encodeURIComponent(n.id)}/shell` }, t('pages.cluster_node.shell')) : null,
          isAdmin() ? button({ size: 'sm', onClick: () => rename(n, reload) }, t('pages.cluster_node.rename')) : null)));
      const placed = [];
      for (const s of st || []) for (const w of s.workloads || []) if (w.node_id === n.id) placed.push({ ...w, stack: s.name, image: ((s.services || {})[w.service] || {}).image });
      return h('div', { class: 'stack' },
        card({ title: t('pages.cluster_node.details'), body: details(n, manager, raft) }),
        card({ title: t('pages.cluster_node.workloads'), body: workloadTable(placed) }),
        card({ title: t('pages.cluster_node.containers'), body: n.containers ? containerTable(n.containers, n) : emptyState(t('pages.cluster_node.no_report'), t('pages.cluster_node.no_report_text')) }));
    },
  });
  const timer = setInterval(() => { if (!document.hidden) reload(); }, EVERY_MS);
  signal.addEventListener('abort', () => clearInterval(timer));
}

function details(n, manager, raft) {
  const r = n.report || {}, l = n.liveness || {}, f = n.fencing || {};
  const hl = l.health || 'unreachable';
  const labels = Object.entries(n.all_labels || r.labels || {}).map(([k, v]) => `${k}=${v}`).join(', ');
  const rows = [
    [t('pages.cluster_node.hostname'), r.hostname || n.hostname || ''],
    [t('pages.cluster_node.id'), h('span', { class: 'mono' }, n.id)],
    [t('pages.cluster.role'), manager ? t('pages.cluster.role_manager') : t('pages.cluster.role_worker')],
    [t('pages.cluster.health'), status({ tone: HEALTH_TONES[hl] || 'info', line: true }, tOr(`pages.cluster.health_word.${hl}`, hl))],
    [t('pages.cluster_node.address'), h('span', { class: 'mono' }, nodeHost(n) || '')],
    [t('pages.cluster.via'), managerLink(heardBy(n, raft))],
    [t('pages.cluster.fencing'), tOr(`pages.cluster.fence.${f.state || 'none'}`, f.state || 'none')],
    [t('pages.cluster.docker'), r.docker_version || ''],
    [t('pages.cluster_node.system'), [r.os, r.kernel, r.arch].filter(Boolean).join(' · ')],
    [t('pages.cluster_node.cpus'), r.cpu_count ? String(r.cpu_count) : ''],
    [t('pages.cluster_node.memory'), r.memory_total_bytes ? bytes(r.memory_total_bytes) : ''],
    [t('pages.cluster.labels'), labels],
    [t('pages.cluster_node.overlay'), overlay(n.network)],
    [t('pages.cluster_node.wg_key'), n.network && n.network.public_key ? h('span', { class: 'mono sub' }, n.network.public_key) : ''],
  ];
  return h('dl', { class: 'fd-facts' }, rows.flatMap(([k, v]) => [h('dt', null, k), h('dd', null, v)]));
}

/** The node's overlay as it last said (G9): applied, failing with why, or not heard of. */
function overlay(net) {
  if (!net) return h('span', { class: 'sub' }, t('pages.cluster_node.overlay_none'));
  if (net.error) return h('div', { class: 'stack' }, status({ tone: 'critical', line: true }, t('pages.cluster_node.overlay_error')), h('span', { class: 'sub' }, net.error));
  if (!net.applied_version) return status({ tone: 'warning', line: true }, t('pages.cluster_node.overlay_waiting'));
  return status({ tone: 'good', line: true }, t('pages.cluster_node.overlay_up', { v: net.applied_version }));
}

function workloadTable(rows) {
  return table({
    rows, filter: false, dense: true, empty: [t('pages.cluster_node.no_workloads'), ''],
    columns: [
      { label: t('pages.cluster.workload_id'), cell: (w) => h('span', { class: 'mono' }, w.id), sort: (w) => w.id },
      { label: t('pages.cluster_node.stack'), cell: (w) => h('a', { href: '/cluster/stacks' }, w.stack), sort: (w) => w.stack },
      { label: t('pages.cluster_stacks.address'), cell: (w) => (w.address ? h('span', { class: 'mono' }, w.address) : ''), sort: (w) => addrKey(w.address) },
      { label: t('pages.cluster.image'), cell: (w) => h('span', { class: 'mono sub' }, w.image || '') },
      { label: t('pages.cluster_stacks.status'), cell: (w) => status({ tone: WORKLOAD_TONES[w.status] || 'info', line: true }, tOr(`pages.cluster_stacks.status_word.${w.status}`, w.status || '')), sort: (w) => w.status },
    ],
  });
}

function containerTable(rows, n) {
  const shells = isAdmin() && (n.report || {}).shells;
  return table({
    rows, filter: false, dense: true, empty: [t('pages.cluster_node.no_containers'), ''],
    columns: [
      { label: t('pages.cluster.workload_id'), cell: (c) => h('span', { class: 'mono' }, c.workload_id), sort: (c) => c.workload_id },
      { label: t('pages.cluster.image'), cell: (c) => h('span', { class: 'mono sub' }, c.image || '') },
      { label: t('pages.cluster_node.state_word'), cell: (c) => status({ tone: STATE_TONES[c.state] || 'info' }, tOr(`pages.cluster_node.state.${c.state}`, c.state || '')) },
      { label: t('pages.cluster_node.container'), cell: (c) => h('span', { class: 'mono sub', title: c.container_id || '' }, shortId(c.container_id || '')) },
      ...(shells ? [{ label: '', nosort: true, cell: (c) => (c.state === 'running' && c.container_id ? h('div', { class: 'row row--end' },
        button({ size: 'sm', href: `/cluster/nodes/${encodeURIComponent(n.id)}/shell?container=${encodeURIComponent(c.container_id)}` }, t('pages.cluster_node.shell'))) : '') }] : []),
    ],
  });
}

function rename(n, reload) {
  nameDialog({
    title: t('pages.cluster_node.rename_title', { name: nodeName(n) }), value: n.name, placeholder: n.hostname, help: t('pages.cluster_node.name_help'),
    save: (name) => put(`/api/cluster/nodes/${encodeURIComponent(n.id)}/name`, { name }), saved: t('pages.cluster_node.renamed'), reload,
  });
}

/** A dialog for a name given in Freightdock only, to a node or a manager. */
export function nameDialog({ title, value, placeholder, help, save, saved, reload }) {
  const f = formField({ id: 'cl-name', label: t('pages.cluster_node.name'), autocomplete: 'off', placeholder: placeholder || '', help });
  f.input.value = value || '';
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, f.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title, body: form, initialFocus: f.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    go.disabled = true;
    try {
      await save(f.input.value);
      m.close();
      toast({ text: saved, tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; f.setError(x.message); }
  });
}
