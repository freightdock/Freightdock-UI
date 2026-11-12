// One manager of the cluster: its place in Raft, whether it is heard, the node on its host if any, and the workers
// whose stream it holds, asked again every few seconds. An administrator gives it a name to be shown by, in
// Freightdock only ("Cluster" 14): the managers know each other by raft id and address.

import { h, mount, bytes } from '../dom.js';
import { get, put } from '../api.js';
import { isAdmin } from '../state.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { t, tOr } from '../i18n.js';
import { nodeName, nodeHost, hostOf, managerName, heardBy } from './cluster.js';
import { nameDialog } from './cluster-node.js';

const EVERY_MS = 3000;
const HEALTH_TONES = { healthy: 'good', suspect: 'warning', unreachable: 'critical' };

export function clusterManagerPage({ el, params, signal }) {
  const head = h('div'), body = h('div');
  el.append(head, body);
  const id = params.id;
  const reload = loadInto(body, {
    signal,
    load: async (s) => {
      const [c, n] = await Promise.all([get('/api/cluster', { signal: s }), get('/api/cluster/nodes', { signal: s }).catch(() => [])]);
      return { c, n };
    },
    draw: ({ c, n }) => {
      const cl = (c || {}).cluster || {};
      const raft = cl.raft || [];
      const m = raft.find((x) => String(x.raft_id) === id);
      if (!m) {
        mount(head, pageHeader(t('pages.cluster_manager.title'), '', button({ size: 'sm', href: '/cluster' }, t('pages.cluster_node.back'))));
        return emptyState(t('pages.cluster_manager.gone'), t('pages.cluster_manager.gone_text'));
      }
      mount(head, pageHeader(managerName(m), m.name ? t('pages.cluster_manager.sub_named', { addr: m.addr }) : t('pages.cluster_manager.sub'),
        h('div', { class: 'row' },
          button({ size: 'sm', href: '/cluster' }, t('pages.cluster_node.back')),
          isAdmin() ? button({ size: 'sm', onClick: () => rename(m, reload) }, t('pages.cluster_node.rename')) : null)));
      const nodes = n || [];
      const local = nodes.find((x) => nodeHost(x) === hostOf(m.addr));
      const heard = nodes.filter((x) => { const b = heardBy(x, raft); return b && b.raft_id === m.raft_id; });
      return h('div', { class: 'stack' },
        card({ title: t('pages.cluster_node.details'), body: details(m, cl, local) }),
        card({ title: t('pages.cluster_manager.workers'), body: workerTable(heard) }));
    },
  });
  const timer = setInterval(() => { if (!document.hidden) reload(); }, EVERY_MS);
  signal.addEventListener('abort', () => clearInterval(timer));
}

function details(m, cl, local) {
  const role = m.leader ? t('pages.cluster.leader') : m.voter ? t('pages.cluster.voter') : t('pages.cluster.learner');
  const rows = [
    [t('pages.cluster_node.address'), h('span', { class: 'mono' }, m.addr || '')],
    [t('pages.cluster.raft_id'), h('span', { class: 'mono' }, String(m.raft_id))],
    [t('pages.cluster.role'), status({ tone: m.leader ? 'good' : m.voter ? 'info' : 'warning' }, role)],
    [t('pages.cluster.online'), status({ tone: m.online ? 'good' : 'critical', line: true }, m.online ? t('pages.cluster.online_yes') : t('pages.cluster.online_no'))],
    [t('pages.cluster_manager.quorum'), cl.quorate ? t('pages.dashboard.quorate') : t('pages.dashboard.not_quorate')],
    [t('pages.cluster_manager.answering'), m.me ? t('pages.cluster_manager.answering_yes') : t('pages.cluster_manager.answering_no')],
    [t('pages.cluster_manager.version'), m.me && cl.version ? cl.version : ''],
    [t('pages.cluster_manager.applied'), m.me && cl.applied_index != null ? String(cl.applied_index) : ''],
    [t('pages.cluster_manager.worker_here'), local ? h('a', { href: `/cluster/nodes/${encodeURIComponent(local.id)}` }, nodeName(local)) : t('pages.cluster.no_worker')],
  ];
  return h('dl', { class: 'fd-facts' }, rows.flatMap(([k, v]) => [h('dt', null, k), h('dd', null, v)]));
}

function workerTable(rows) {
  return table({
    rows, filter: false, dense: true, empty: [t('pages.cluster_manager.no_workers'), ''],
    columns: [
      { label: t('pages.cluster.node'), cell: (n) => h('a', { href: `/cluster/nodes/${encodeURIComponent(n.id)}` }, nodeName(n)), sort: (n) => nodeName(n) },
      { label: t('pages.cluster_node.address'), cell: (n) => h('span', { class: 'mono sub' }, nodeHost(n)) },
      { label: t('pages.cluster.health'), cell: (n) => { const hl = (n.liveness || {}).health || 'unreachable'; return status({ tone: HEALTH_TONES[hl] || 'info', line: true }, tOr(`pages.cluster.health_word.${hl}`, hl)); } },
      { label: t('pages.cluster_node.memory'), cell: (n) => { const r = n.report || {}; return r.memory_total_bytes ? bytes(r.memory_total_bytes) : ''; } },
    ],
  });
}

function rename(m, reload) {
  nameDialog({
    title: t('pages.cluster_node.rename_title', { name: managerName(m) }), value: m.name, placeholder: hostOf(m.addr), help: t('pages.cluster_manager.name_help'),
    save: (name) => put(`/api/cluster/managers/${encodeURIComponent(m.raft_id)}/name`, { name }), saved: t('pages.cluster_manager.renamed'), reload,
  });
}
