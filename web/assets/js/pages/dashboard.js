// The dashboard ("Cluster" 13): the cluster at a glance, asked again every few seconds. On top the managers (how many
// are online), the quorum (and which node leads) and the workers (how many are healthy); under them the CPU and memory
// used over every node; under those a row per node with its resources and its workloads.

import { h, s, mount, bytes, num } from '../dom.js';
import { get } from '../api.js';
import { loadInto } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { meter } from '../../finui/components/meter/meter.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { t, tOr } from '../i18n.js';
import { nodeName, nodeHost, managerHosts, managerLink } from './cluster.js';

const EVERY_MS = 3000;
const HEALTH_TONES = { healthy: 'good', suspect: 'warning', unreachable: 'critical' };

/** A half-round gauge for a share from 0 to 1, in the tone given, the value written under it. */
function gauge(share, tone, value, caption) {
  const p = Math.max(0, Math.min(1, Number(share) || 0)) * 100;
  const arc = 'M 10 50 A 40 40 0 0 1 90 50';
  // The value sits inside the arch, the caption under it.
  return h('div', { class: 'fd-gauge' },
    h('div', { class: 'fd-gauge__dial' },
      s('svg', { viewBox: '0 0 100 56', class: 'fd-gauge__svg', 'aria-hidden': 'true' },
        s('path', { d: arc, class: 'fd-gauge__track', pathLength: '100' }),
        s('path', { d: arc, class: `fd-gauge__fill fd-gauge__fill--${tone}`, pathLength: '100', 'stroke-dasharray': `${p} 100` })),
      h('div', { class: `fd-gauge__value fd-gauge__value--${tone}` }, value)),
    h('div', { class: 'fd-gauge__caption' }, caption));
}

/** The managers: good when all are online, warning while one more could go and quorum hold, orange when one more
 *  would lose it, critical without it. */
function managerTone(up, voters, quorate) {
  if (!quorate || !voters) return 'critical';
  if (up >= voters) return 'good';
  return (up - 1) * 2 > voters ? 'warning' : 'orange';
}
/** The workers (no quorum): good when all are healthy, warning when some are not, orange with one left, critical with
 *  none. */
function workerTone(up, all) {
  if (!all || !up) return 'critical';
  if (up === all) return 'good';
  return up > 1 ? 'warning' : 'orange';
}
/** For what is used: good under 70%, warning under 90%. */
const useTone = (share) => (share < 0.7 ? 'good' : share < 0.9 ? 'warning' : 'critical');

export function dashboardPage({ el, signal }) {
  const head = h('div'), body = h('div');
  el.append(head, body);
  mount(head, pageHeader(t('pages.dashboard.title'), t('pages.dashboard.sub')));
  const reload = loadInto(body, {
    signal,
    load: async (sg) => {
      const [c, n, st] = await Promise.all([get('/api/cluster', { signal: sg }), get('/api/cluster/nodes', { signal: sg }).catch(() => []), get('/api/cluster/stacks', { signal: sg }).catch(() => [])]);
      return { c, n, st };
    },
    draw: ({ c, n, st }) => {
      if (!c.connected) return emptyState(t('pages.cluster.not_connected'), t('pages.cluster.not_connected_text'));
      const cl = c.cluster || {};
      const raft = cl.raft || [];
      const nodes = n || [];
      return h('div', { class: 'stack' }, topRow(cl, raft, nodes), usageRow(nodes), nodeRows(nodes, st || [], raft));
    },
  });
  const timer = setInterval(() => { if (!document.hidden) reload(); }, EVERY_MS);
  signal.addEventListener('abort', () => clearInterval(timer));
}

function topRow(cl, raft, nodes) {
  const up = raft.filter((m) => m.online).length;
  const voters = raft.filter((m) => m.voter).length;
  const votersUp = raft.filter((m) => m.voter && m.online).length;
  const leader = raft.find((m) => m.leader);
  const healthy = nodes.filter((x) => (x.liveness || {}).health === 'healthy').length;
  return h('div', { class: 'fd-dash-row' },
    card({ title: t('pages.dashboard.managers'), body: gauge(raft.length ? up / raft.length : 0, managerTone(votersUp, voters, cl.quorate), t('pages.dashboard.n_of', { n: up, of: raft.length }), t('pages.dashboard.online')) }),
    card({ title: t('pages.dashboard.quorum'), body: h('div', { class: 'fd-gauge' },
      h('div', { class: 'fd-dash-quorum' }, cl.quorate ? status({ tone: managerTone(votersUp, voters, true) === 'good' ? 'good' : 'warning' }, t('pages.dashboard.quorate')) : status({ tone: 'critical' }, t('pages.dashboard.not_quorate'))),
      h('div', { class: 'fd-gauge__caption' }, t('pages.dashboard.voters', { n: voters })),
      h('div', { class: 'fd-gauge__value' }, leader ? managerLink(leader) : t('pages.dashboard.no_leader')),
      h('div', { class: 'fd-gauge__caption' }, t('pages.dashboard.leader'))) }),
    card({ title: t('pages.dashboard.workers'), body: gauge(nodes.length ? healthy / nodes.length : 0, workerTone(healthy, nodes.length), t('pages.dashboard.n_of', { n: healthy, of: nodes.length }), t('pages.dashboard.healthy')) }));
}

/** CPU over every node weighted by its CPUs, memory as used over total; nodes not heard are left out. */
function usageRow(nodes) {
  let cpus = 0, busy = 0, memAll = 0, memUsed = 0;
  for (const x of nodes) {
    const l = x.liveness || {}, r = x.report || {};
    if (l.cpu_percent != null && r.cpu_count) { cpus += r.cpu_count; busy += (l.cpu_percent / 100) * r.cpu_count; }
    if (l.memory_used_bytes != null && r.memory_total_bytes) { memAll += r.memory_total_bytes; memUsed += l.memory_used_bytes; }
  }
  const cpu = cpus ? busy / cpus : 0, mem = memAll ? memUsed / memAll : 0;
  return h('div', { class: 'fd-dash-row fd-dash-row--two' },
    card({ title: t('pages.dashboard.cpu'), body: gauge(cpu, useTone(cpu), t('units.percent', { n: num(cpu * 100, 1) }), t('pages.dashboard.cpu_of', { n: cpus })) }),
    card({ title: t('pages.dashboard.memory'), body: gauge(mem, useTone(mem), t('units.percent', { n: num(mem * 100, 1) }), t('pages.dashboard.memory_of', { used: bytes(memUsed), all: bytes(memAll) })) }));
}

function bar(share, text) {
  return h('div', { class: 'fd-dash-bar' }, meter({ value: share, block: true, label: text }), h('span', { class: 'sub nowrap' }, text));
}

function nodeRows(nodes, stacks, raft) {
  const hosts = managerHosts(raft);
  const work = {};
  for (const st of stacks) for (const w of st.workloads || []) {
    if (!w.node_id) continue;
    const c = (work[w.node_id] ||= { all: 0, running: 0, bad: 0 });
    c.all += 1;
    if (w.status === 'running') c.running += 1;
    else if (['failed', 'fenced', 'unknown'].includes(w.status)) c.bad += 1;
  }
  const rows = [...nodes].sort((a, b) => nodeName(a).localeCompare(nodeName(b)));
  if (!rows.length) return card({ title: t('pages.dashboard.nodes'), body: emptyState(t('pages.cluster.no_nodes'), t('pages.cluster.no_nodes_text')) });
  return card({ title: t('pages.dashboard.nodes'), body: h('div', { class: 'fd-dash-nodes' }, rows.map((x) => {
    const l = x.liveness || {}, r = x.report || {};
    const hl = l.health || 'unreachable';
    const cpu = l.cpu_percent != null ? l.cpu_percent / 100 : 0;
    const mem = r.memory_total_bytes && l.memory_used_bytes != null ? l.memory_used_bytes / r.memory_total_bytes : 0;
    const disk = l.disk_total_bytes ? (l.disk_used_bytes || 0) / l.disk_total_bytes : 0;
    const w = work[x.id] || { all: 0, running: 0, bad: 0 };
    return h('div', { class: 'fd-dash-node' },
      h('div', { class: 'fd-dash-node__name' },
        h('a', { href: `/cluster/nodes/${encodeURIComponent(x.id)}` }, nodeName(x)),
        h('span', { class: 'sub' }, hosts.has(nodeHost(x)) ? t('pages.cluster.role_manager') : t('pages.cluster.role_worker')),
        status({ tone: HEALTH_TONES[hl] || 'info', line: true }, tOr(`pages.cluster.health_word.${hl}`, hl))),
      h('div', { class: 'fd-dash-node__res' },
        h('span', { class: 'sub' }, t('pages.dashboard.cpu')), bar(cpu, l.cpu_percent != null ? t('units.percent', { n: num(l.cpu_percent, 1) }) : ''),
        h('span', { class: 'sub' }, t('pages.dashboard.memory')), bar(mem, r.memory_total_bytes ? `${bytes(l.memory_used_bytes || 0)} / ${bytes(r.memory_total_bytes)}` : ''),
        h('span', { class: 'sub' }, t('pages.dashboard.disk')), bar(disk, l.disk_total_bytes ? `${bytes(l.disk_used_bytes || 0)} / ${bytes(l.disk_total_bytes)}` : '')),
      h('div', { class: 'fd-dash-node__work' },
        status({ tone: w.bad ? 'critical' : w.running === w.all ? 'good' : 'warning' }, t('pages.dashboard.workloads_n', { n: w.running, of: w.all })),
        h('span', { class: 'sub' }, t('pages.dashboard.containers_n', { n: l.containers_running ?? 0 }))));
  })) });
}
