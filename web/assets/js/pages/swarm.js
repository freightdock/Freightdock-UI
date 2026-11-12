// Swarm: the cluster at a glance (its nodes, services and tasks), and the nodes page: each node's state, role and
// availability, and for an administrator draining, pausing, promoting, demoting, labelling and removing it.

import { h, ago, when, mount, num, bytes } from '../dom.js';
import { get, put, del, qs } from '../api.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { confirm } from '../confirm.js';
import { sbase, swarmPage, taskBadge, nodeBadges, nodeName, replicas, serviceImage, stackOf } from '../swarm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { statTile } from '../../finui/components/stat-tile/stat-tile.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { button } from '../../finui/components/button/button.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { openDrawer } from '../../finui/components/drawer/drawer.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { textArea } from '../../finui/components/field/field.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t, tOr } from '../i18n.js';

const labelText = (l) => Object.entries(l || {}).map(([k, v]) => (v ? `${k}=${v}` : k)).join('\n');
const toLabels = (s) => Object.fromEntries(s.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => { const i = l.indexOf('='); return i < 0 ? [l, ''] : [l.slice(0, i).trim(), l.slice(i + 1).trim()]; }));

/** The tasks as a table: which service, which node, its state and since when. */
export function tasksTable(tasks, { services = {}, nodes = {}, empty = [t('pages.swarm.no_tasks'), ''] } = {}) {
  return table({
    rows: tasks.slice().sort((a, b) => String(b.UpdatedAt).localeCompare(String(a.UpdatedAt))), dense: true, empty,
    columns: [
      { label: t('pages.swarm.task'), cell: (task) => h('span', { class: 'mono' }, `${services[task.ServiceID] || task.ServiceID.slice(0, 12)}.${task.Slot || (task.NodeID || '').slice(0, 6)}`) },
      { label: t('common.state'), cell: (task) => taskBadge(task), sort: (task) => (task.Status || {}).State },
      { label: t('pages.swarm.wanted'), cell: (task) => h('span', { class: 'faint' }, tOr(`swarm.task_state.${task.DesiredState}`, task.DesiredState)) },
      { label: t('pages.swarm.node'), cell: (task) => nodes[task.NodeID] || h('span', { class: 'faint' }, task.NodeID ? task.NodeID.slice(0, 12) : t('pages.swarm.none_yet')) },
      { label: t('pages.swarm.since'), cell: (task) => h('span', { class: 'nowrap' }, ago(Date.parse((task.Status || {}).Timestamp || task.UpdatedAt) / 1000)), sort: (task) => Date.parse((task.Status || {}).Timestamp || task.UpdatedAt) },
      { label: t('pages.swarm.said'), cell: (task) => h('span', { class: 'muted' }, (task.Status || {}).Err || (task.Status || {}).Message || '') },
    ],
  });
}

const tile = (label, value, hint, href) => h('a', { class: 'tile-link', href }, statTile({ label, value, hint }));

export const clusterPage = swarmPage(({ el, signal, every }) => {
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: async (s) => {
      const [sw, nodes, services, tasks] = await Promise.all([get(sbase(), { signal: s }), get(sbase('/nodes'), { signal: s }), get(sbase('/services'), { signal: s }), get(sbase('/tasks?state=running'), { signal: s })]);
      return { sw, nodes, services, tasks };
    },
    draw: ({ sw, nodes, services, tasks }) => {
      const ready = nodes.filter((n) => (n.Status || {}).State === 'ready').length;
      const managers = nodes.filter((n) => (n.Spec || {}).Role === 'manager').length;
      const running = tasks.filter((task) => (task.Status || {}).State === 'running').length;
      const short = services.filter((s) => (s.ServiceStatus || {}).RunningTasks !== (s.ServiceStatus || {}).DesiredTasks).length;
      const names = Object.fromEntries(services.map((s) => [s.ID, s.Spec.Name]));
      const hosts = Object.fromEntries(nodes.map((n) => [n.ID, nodeName(n)]));
      const stacks = new Set(services.map(stackOf).filter(Boolean));
      const swarm = sw.swarm || {};
      return h('div', { class: 'stack' },
        h('div', { class: 'fui-stat-tile__grid' },
          tile(t('nav.nodes'), num(nodes.length), t('pages.swarm.nodes_hint', { ready, count: managers }), '/swarm/nodes'),
          tile(t('nav.services'), num(services.length), short ? t('pages.swarm.short', { count: short }) : t('pages.swarm.at_count'), '/swarm/services'),
          tile(t('pages.swarm.tasks'), num(running), t('pages.swarm.running_now'), '/swarm/services'),
          tile(t('nav.stacks'), num(stacks.size), t('pages.swarm.with_docker_stack'), '/swarm/stacks')),
        h('div', { class: 'grid-2' },
          card({ title: t('nav.nodes'), body: table({ rows: nodes, dense: true, filter: false, empty: [t('pages.swarm.no_nodes'), ''], columns: [
            { label: t('pages.swarm.node'), cell: (n) => h('span', { class: 'name-cell' }, h('span', null, nodeName(n)), h('span', { class: 'sub' }, (n.Status || {}).Addr || '')) },
            { label: t('pages.swarm.role'), cell: (n) => ((n.ManagerStatus || {}).Leader ? t('pages.swarm.leader') : tOr(`pages.swarm.roles.${n.Spec.Role}`, n.Spec.Role)) },
            { label: t('common.state'), cell: (n) => nodeBadges(n) },
          ] }) }),
          card({ title: t('nav.services'), body: table({ rows: services, dense: true, filter: false, empty: [t('pages.stack.no_services'), t('pages.swarm.no_services_text')], columns: [
            { label: t('pages.stack.service'), cell: (s) => h('a', { href: `/swarm/services/${encodeURIComponent(s.Spec.Name)}` }, s.Spec.Name) },
            { label: t('pages.swarm.tasks'), cell: (s) => replicas(s) },
            { label: t('pages.containers.image'), cell: (s) => h('span', { class: 'mono faint truncate' }, serviceImage(s)) },
          ] }) })),
        card({ title: t('pages.swarm.tasks_running'), body: tasksTable(tasks, { services: names, nodes: hosts, empty: [t('pages.containers.none_running'), t('pages.swarm.no_task_running')] }) }),
        card({ title: t('pages.swarm.the_swarm'), body: facts([['Id', (swarm.id || '').slice(0, 12), { mono: true }], [t('common.made'), swarm.created ? when(Date.parse(swarm.created) / 1000) : null], [t('pages.swarm.this_node'), sw.node_addr, { mono: true }], [t('pages.swarm.managers'), num(sw.managers ?? managers)]]) }));
    },
  });
  every(10000, reload);
  el.append(pageHeader(t('nav.swarm'), t('pages.swarm.sub')), slot);
});

function labelsDialog(n, after) {
  const field = textArea({ id: 'node-labels', label: t('common.labels'), rows: 6, help: t('pages.swarm.labels_help') });
  field.input.value = labelText(n.Spec.Labels);
  const fail = h('p', { class: 'fui-field__error', role: 'alert' });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, field.el, fail, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.swarm.labels_of', { name: nodeName(n) }), body: form, initialFocus: field.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); mount(fail);
    try { await put(sbase(`/nodes/${encodeURIComponent(n.ID)}`), { labels: toLabels(field.input.value) }); m.close(); toast({ text: t('common.saved'), tone: 'good' }); after(); }
    catch (err) { mount(fail, err.message); }
  });
}

async function change(n, body, words, after) {
  try { await put(sbase(`/nodes/${encodeURIComponent(n.ID)}`), body); toast({ text: words, tone: 'good' }); after(); }
  catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

async function removeNode(n, after) {
  const up = (n.Status || {}).State !== 'down';
  const ok = await confirm({ title: t('pages.swarm.remove_title', { name: nodeName(n) }), body: up
    ? t('pages.swarm.remove_up')
    : t('pages.swarm.remove_down'), option: up ? { label: t('pages.images.force'), help: t('pages.swarm.force_help') } : null });
  if (!ok) return;
  try { await del(sbase(`/nodes/${encodeURIComponent(n.ID)}${qs({ force: ok.option ? 'true' : '' })}`)); toast({ text: t('common.removed'), tone: 'good' }); after(); }
  catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

function nodeActions(n, after) {
  const av = n.Spec.Availability, role = n.Spec.Role;
  return dropdownMenu({ label: t('pages.swarm.change', { name: nodeName(n) }), icon: 'moreHorizontal', iconOnly: true, size: 'sm', items: [
    av !== 'active' ? { label: t('pages.swarm.make_active'), icon: 'play', onSelect: () => change(n, { availability: 'active' }, t('pages.swarm.active_done'), after) } : null,
    av !== 'pause' ? { label: t('pages.containers.pause'), icon: 'pause', onSelect: () => change(n, { availability: 'pause' }, t('pages.swarm.paused_done'), after) } : null,
    av !== 'drain' ? { label: t('pages.swarm.drain'), icon: 'arrowDown', onSelect: async () => {
      if (await confirm({ title: t('pages.swarm.drain_title', { name: nodeName(n) }), body: t('pages.swarm.drain_body'), action: t('pages.swarm.drain'), danger: false })) change(n, { availability: 'drain' }, t('pages.swarm.draining'), after);
    } } : null,
    { separator: true },
    role === 'worker' ? { label: t('pages.swarm.promote'), icon: 'arrowUp', onSelect: () => change(n, { role: 'manager' }, t('pages.swarm.promoted'), after) } : null,
    role === 'manager' ? { label: t('pages.swarm.demote_to_worker'), icon: 'arrowDown', onSelect: async () => {
      if (await confirm({ title: t('pages.swarm.demote_title', { name: nodeName(n) }), body: t('pages.swarm.demote_body'), action: t('pages.swarm.demote'), danger: false })) change(n, { role: 'worker' }, t('pages.swarm.demoted'), after);
    } } : null,
    { label: t('common.labels'), icon: 'tag', onSelect: () => labelsDialog(n, after) },
    { separator: true },
    { label: t('common.remove'), icon: 'trash', danger: true, onSelect: () => removeNode(n, after) },
  ].filter(Boolean) });
}

async function nodeDetails(n) {
  const d = await get(sbase(`/nodes/${encodeURIComponent(n.ID)}`));
  const node = d.node, desc = node.Description || {}, res = desc.Resources || {};
  openDrawer({ title: nodeName(node), body: h('div', { class: 'stack' },
    facts([[t('pages.swarm.role'), tOr(`pages.swarm.roles.${node.Spec.Role}`, node.Spec.Role)], [t('pages.swarm.availability'), tOr(`swarm.availability.${node.Spec.Availability}`, node.Spec.Availability)], [t('common.state'), tOr(`swarm.node_state.${(node.Status || {}).State}`, (node.Status || {}).State)], [t('pages.environments.address'), (node.Status || {}).Addr, { mono: true }],
      ['Docker', (desc.Engine || {}).EngineVersion], [t('pages.dashboard.system'), `${(desc.Platform || {}).OS || ''} ${(desc.Platform || {}).Architecture || ''}`],
      [t('pages.dashboard.cpus'), res.NanoCPUs ? num(res.NanoCPUs / 1e9) : null], [t('pages.dashboard.memory'), res.MemoryBytes ? bytes(res.MemoryBytes) : null],
      [t('pages.swarm.manager'), node.ManagerStatus ? (node.ManagerStatus.Leader ? t('pages.swarm.leader_reach', { reach: node.ManagerStatus.Reachability }) : node.ManagerStatus.Reachability) : null],
      [t('common.labels'), labelText(node.Spec.Labels) || t('common.none_word'), { wide: true, mono: true }], ['Id', node.ID, { wide: true, mono: true }]]),
    h('h3', { class: 'drawer-sub' }, t('pages.swarm.its_tasks')),
    tasksTable(d.tasks.filter((task) => task.DesiredState !== 'shutdown'), { empty: [t('pages.swarm.no_tasks'), t('pages.swarm.nothing_placed')] })) });
}

export const nodesPage = swarmPage(({ el, signal, every }) => {
  let words = [];
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(sbase('/nodes'), { signal: s }),
    draw: (list, again) => table({
      rows: list.filter((n) => matches(words, nodeName(n), n.Spec.Role, n.Spec.Availability, (n.Status || {}).Addr, labelText(n.Spec.Labels))), filter: false, empty: [t('pages.swarm.no_nodes'), ''],
      columns: [
        { label: t('pages.swarm.node'), cell: (n) => h('div', { class: 'name-cell' }, h('button', { type: 'button', class: 'link-button', onClick: () => nodeDetails(n).catch((e) => toast({ text: e.message, tone: 'critical' })) }, nodeName(n)), h('span', { class: 'sub mono' }, n.ID.slice(0, 12))) },
        { label: t('pages.swarm.role'), cell: (n) => h('span', { class: 'row' }, tOr(`pages.swarm.roles.${n.Spec.Role}`, n.Spec.Role), (n.ManagerStatus || {}).Leader ? status({ tone: 'info', line: true }, t('pages.swarm.leader')) : null) },
        { label: t('common.state'), cell: (n) => nodeBadges(n), sort: (n) => `${(n.Status || {}).State}${n.Spec.Availability}` },
        { label: t('pages.environments.address'), cell: (n) => h('span', { class: 'mono' }, (n.Status || {}).Addr || '') },
        { label: 'Docker', cell: (n) => ((n.Description || {}).Engine || {}).EngineVersion || '' },
        { label: t('common.labels'), cell: (n) => h('span', { class: 'row' }, Object.entries(n.Spec.Labels || {}).map(([k, v]) => h('span', { class: 'fui-chip mono' }, v ? `${k}=${v}` : k))) },
        { label: '', nosort: true, cell: (n) => (can('administer') ? h('div', { class: 'cell-actions' }, nodeActions(n, again)) : '') },
      ],
    }),
  });
  every(10000, reload);
  el.append(pageHeader(t('nav.nodes'), t('pages.swarm.nodes_sub')), h('div', { class: 'toolbar' }, filterBox(t('pages.swarm.filter_nodes'), (w) => { words = w; reload.redraw(); })), slot);
});
