// Containers: every one on the host, its state, image, stack, ports and what it uses now, with the everyday actions in a
// menu on each row. Refreshed every few seconds while the page is open and the tab is seen.

import { h, ago, bytes, when, pct } from '../dom.js';
import { get, post, del, qs } from '../api.js';
import { base, containerName, stateBadge, healthOf, portLinks, projectOf, updateBadge } from '../docker.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { confirm } from '../confirm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { button } from '../../finui/components/button/button.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t, lang } from '../i18n.js';

/** Run an action on a container and say how it went. */
export async function act(id, name, action, after = () => {}) {
  try {
    await post(base(`/containers/${encodeURIComponent(id)}/${action}`));
    toast({ text: t(`pages.containers.done.${action}`, { name }), tone: 'good' });
    after();
  } catch (e) { toast({ text: t('common.name_said', { name, message: e.message }), tone: 'critical' }); }
}

export async function removeContainer(id, name, running, after = () => {}) {
  const ok = await confirm({ title: t('common.remove_name', { name }), body: running ? t('pages.containers.remove_running') : t('pages.containers.remove_body'),
    option: { label: t('pages.containers.anon_volumes'), help: t('pages.containers.anon_volumes_help') } });
  if (!ok) return false;
  try {
    await del(base(`/containers/${encodeURIComponent(id)}${qs({ force: running ? 1 : '', volumes: ok.option ? 1 : '' })}`));
    toast({ text: t('pages.containers.removed', { name }), tone: 'good' });
    after();
    return true;
  } catch (e) { toast({ text: e.message, tone: 'critical' }); return false; }
}

export async function recreate(id, name, after = () => {}) {
  const ok = await confirm({ title: t('pages.containers.recreate_title', { name }), danger: false, action: t('pages.containers.recreate'),
    body: t('pages.containers.recreate_body'),
    option: { label: t('pages.containers.pull_first'), help: t('pages.containers.pull_first_help') } });
  if (!ok) return;
  toast({ text: t('pages.containers.recreating', { name }) });
  try {
    await post(base(`/containers/${encodeURIComponent(id)}/recreate`), { pull: ok.option });
    toast({ text: t('pages.containers.recreated', { name }), tone: 'good' });
    after();
  } catch (e) { toast({ text: t('common.name_said', { name, message: e.message }), tone: 'critical' }); }
}

function rowMenu(c, reload) {
  const name = containerName(c), id = c.Id, running = c.State === 'running', paused = c.State === 'paused';
  const ref = encodeURIComponent(name);
  const items = [
    { label: t('pages.containers.logs'), icon: 'log', href: `/containers/${ref}?tab=logs` },
    running && can('console') ? { label: t('pages.containers.console'), icon: 'terminal', href: `/containers/${ref}?tab=console` } : null,
    { label: t('pages.containers.stats'), icon: 'chart', href: `/containers/${ref}?tab=stats` },
  ];
  if (can('operate')) items.push({ separator: true },
    !running && !paused ? { label: t('pages.containers.start'), icon: 'play', onSelect: () => act(id, name, 'start', reload) } : null,
    running ? { label: t('pages.containers.stop'), icon: 'stop', onSelect: () => act(id, name, 'stop', reload) } : null,
    running || paused ? { label: t('pages.containers.restart'), icon: 'refresh', onSelect: () => act(id, name, 'restart', reload) } : null,
    running ? { label: t('pages.containers.pause'), icon: 'pause', onSelect: () => act(id, name, 'pause', reload) } : null,
    paused ? { label: t('pages.containers.resume'), icon: 'play', onSelect: () => act(id, name, 'unpause', reload) } : null,
    running ? { label: t('pages.containers.kill'), icon: 'zap', onSelect: () => act(id, name, 'kill', reload) } : null);
  if (can('manage')) items.push({ separator: true },
    { label: t('common.edit'), icon: 'pencil', href: `/containers/${ref}/edit` },
    can('create') ? { label: t('pages.containers.duplicate'), icon: 'copy', href: `/containers/new?from=${ref}` } : null,
    { label: t('pages.containers.recreate'), icon: 'repeat', onSelect: () => recreate(id, name, reload) },
    { separator: true },
    { label: t('common.remove'), icon: 'trash', danger: true, onSelect: () => removeContainer(id, name, running, reload) });
  return dropdownMenu({ label: t('common.actions_for', { name }), items, icon: 'moreHorizontal', iconOnly: true, variant: 'ghost', size: 'sm' });
}

export function containersPage({ el, signal, every, query }) {
  let show = query.get('show') === 'running' ? 'running' : 'all';
  let stats = {}, updates = {}, words = [];
  const slot = h('div');
  const draw = (list, reload) => {
    const rows = list.filter((c) => (show === 'all' || c.State === 'running') && matches(words, containerName(c), c.Image, projectOf(c), c.Status)).sort((a, b) => containerName(a).localeCompare(containerName(b), lang()));
    return table({
      rows, filter: false, empty: show === 'running' ? [t('pages.containers.none_running'), t('pages.containers.none_running_text')] : [t('pages.containers.empty'), t('pages.containers.empty_text')],
      rowClass: (c) => (c.State !== 'running' ? 'is-quiet' : null),
      columns: [
        { label: t('common.name'), cell: (c) => h('div', { class: 'name-cell' }, h('a', { href: `/containers/${encodeURIComponent(containerName(c))}` }, containerName(c)), h('span', { class: 'sub mono' }, c.Id.slice(0, 12))) },
        { label: t('common.state'), cell: (c) => h('div', { class: 'name-cell' }, stateBadge(c.State, healthOf(c.Status)), h('span', { class: 'sub' }, c.Status)), sort: (c) => c.State },
        { label: t('pages.containers.image'), cell: (c) => h('span', { class: 'row' }, h('span', { class: 'mono truncate', title: c.Image }, c.Image), updateBadge(updates[c.Id])) },
        { label: t('routes.stack'), cell: (c) => projectOf(c) || h('span', { class: 'faint' }, '') },
        { label: t('pages.containers.ports'), cell: (c) => portLinks(c.Ports), nosort: true },
        { label: 'CPU', cls: 'r', cell: (c) => (stats[c.Id] ? h('span', { class: 'mono' }, pct(stats[c.Id].cpu)) : ''), sort: (c) => (stats[c.Id] ? stats[c.Id].cpu : -1) },
        { label: t('pages.dashboard.memory'), cls: 'r', cell: (c) => (stats[c.Id] ? h('span', { class: 'mono' }, bytes(stats[c.Id].mem)) : ''), sort: (c) => (stats[c.Id] ? stats[c.Id].mem : -1) },
        { label: t('common.made'), cell: (c) => h('span', { class: 'nowrap', title: when(c.Created) }, ago(c.Created)), sort: (c) => c.Created },
        { label: '', cell: (c) => h('div', { class: 'cell-actions' }, rowMenu(c, reload)), nosort: true },
      ],
    });
  };
  const reload = loadInto(slot, {
    signal,
    // The list comes at once; what each uses takes Docker a second or two to sample, and is drawn in when it comes.
    load: (s) => {
      get(base('/stats'), { signal: s }).then((st) => { stats = st; reload.redraw(); }, () => {});
      get(base('/updates'), { signal: s }).then((u) => { updates = u.containers || {}; reload.redraw(); }, () => {});
      return get(base('/containers'), { signal: s });
    },
    draw,
  });
  const filter = segmented({ label: t('pages.events.show'), value: show, size: 'sm', options: [{ value: 'all', label: t('pages.containers.all') }, { value: 'running', label: t('pages.containers.running') }],
    onChange: (v) => { show = v; history.replaceState(null, '', v === 'running' ? '?show=running' : location.pathname); reload.redraw(); } });
  const actions = h('div', { class: 'row' },
    can('administer') ? button({ size: 'sm', onClick: async () => {
      const ok = await confirm({ title: t('pages.containers.prune_title'), body: t('pages.containers.prune_body') });
      if (!ok) return;
      try { const r = await post(base('/containers/prune')); toast({ text: t('common.n_removed', { count: (r.ContainersDeleted || []).length }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
    } }, icon('trash', 13), t('pages.containers.remove_stopped')) : null,
    can('create') ? button({ variant: 'primary', size: 'sm', href: '/containers/new' }, icon('plus', 14), t('routes.add_container')) : null);
  el.append(pageHeader(t('nav.containers'), t('pages.containers.sub'), actions), h('div', { class: 'toolbar' }, filter, filterBox(t('pages.containers.filter'), (w) => { words = w; reload.redraw(); })), slot);
  every(5000, reload);
}
