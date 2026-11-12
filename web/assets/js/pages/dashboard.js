// The dashboard: what the environment holds, the host it runs on, the disk Docker uses, and what just happened.

import { h, bytes, ago, num, when } from '../dom.js';
import { get, socket } from '../api.js';
import { base, containerName } from '../docker.js';
import { loadInto } from '../view.js';
import { can } from '../state.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { statTile } from '../../finui/components/stat-tile/stat-tile.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { meter } from '../../finui/components/meter/meter.js';
import { sk } from '../../finui/components/skeleton/skeleton.js';
import { button } from '../../finui/components/button/button.js';
import { icon } from '../../finui/core.js';
import { prune } from './prune.js';
import { t } from '../i18n.js';

const tile = (label, value, hint, href) => {
  const tileEl = statTile({ label, value, hint });
  tileEl.classList.add('fm-roll');
  return href ? h('a', { class: 'tile-link', href }, tileEl) : tileEl;
};

function overview(d) {
  const { info, version } = d.info;
  const imgs = d.images, vols = d.volumes, nets = d.networks;
  const running = info.ContainersRunning || 0, stopped = info.ContainersStopped || 0, paused = info.ContainersPaused || 0;
  const tiles = h('div', { class: 'fui-stat-tile__grid' },
    tile(t('nav.containers'), num(info.Containers || 0), paused ? t('pages.dashboard.containers_paused', { running, stopped, paused }) : t('pages.dashboard.containers_hint', { running, stopped }), '/containers'),
    tile(t('nav.images'), num(imgs.length), bytes(d.df.images), '/images'),
    tile(t('nav.volumes'), num(vols.length), bytes(d.df.volumes), '/volumes'),
    tile(t('nav.networks'), num(nets.length), t('pages.dashboard.own_networks', { count: nets.filter((n) => !n.built_in).length }), '/networks'));
  const total = d.df.images + d.df.containers + d.df.volumes + d.df.build_cache;
  const disk = [[t('nav.images'), d.df.images], [t('nav.volumes'), d.df.volumes], [t('pages.dashboard.build_cache'), d.df.build_cache], [t('nav.containers'), d.df.containers]];
  const host = card({ title: info.Name || t('pages.dashboard.this_host'), sub: `Docker ${version.Version} · API ${version.ApiVersion}`, body: facts([
    [t('pages.dashboard.system'), info.OperatingSystem], [t('pages.dashboard.kernel'), info.KernelVersion], [t('pages.dashboard.architecture'), info.Architecture],
    [t('pages.dashboard.cpus'), String(info.NCPU)], [t('pages.dashboard.memory'), bytes(info.MemTotal)], [t('pages.dashboard.storage_driver'), info.Driver],
    [t('pages.dashboard.logging'), info.LoggingDriver], ['Cgroup', `v${info.CgroupVersion} (${info.CgroupDriver})`],
    [t('pages.dashboard.data_root'), info.DockerRootDir, { wide: true, mono: true }],
    info.Swarm && info.Swarm.LocalNodeState === 'active' ? [t('nav.swarm'), t('swarm.availability.active')] : null,
  ]) });
  const diskCard = card({ title: t('pages.dashboard.disk'), sub: t('pages.dashboard.in_all', { size: bytes(total) }),
    actions: can('administer') ? button({ size: 'sm', onClick: () => prune() }, icon('trash', 13), t('common.clean_up')) : null,
    body: h('div', { class: 'disk' }, disk.map(([k, v]) => h('div', { class: 'disk__row' },
      h('span', { class: 'disk__name' }, k), meter({ value: total ? v / total : 0, block: true, label: k }), h('span', { class: 'disk__size mono' }, bytes(v))))) });
  return h('div', { class: 'stack' }, tiles, h('div', { class: 'grid-2' }, host, diskCard));
}

function eventsCard({ cleanup }) {
  const list = h('ul', { class: 'events' });
  const empty = h('p', { class: 'muted' }, t('pages.dashboard.nothing_happened'));
  const body = h('div', null, empty, list);
  const ws = socket(base(`/events/ws?since=${Math.floor(Date.now() / 1000) - 3600}`));
  ws.onmessage = (e) => {
    let ev; try { ev = JSON.parse(e.data); } catch { return; }
    if (!ev.Action || ev.Action.startsWith('exec_') || ev.Action.startsWith('health_status')) return;
    empty.remove();
    const a = ev.Actor || {}, attrs = a.Attributes || {};
    const name = attrs.name || (a.ID || '').slice(0, 12);
    const href = ev.Type === 'container' ? `/containers/${encodeURIComponent(name)}` : ev.Type === 'image' ? null : null;
    list.prepend(h('li', { class: 'events__row' },
      h('span', { class: 'events__when faint', title: when(ev.time || 0) }, ago(ev.time)),
      h('span', { class: 'fui-chip' }, ev.Type), h('strong', null, ev.Action),
      href ? h('a', { class: 'mono truncate', href }, name) : h('span', { class: 'mono truncate' }, name)));
    while (list.children.length > 12) list.lastChild.remove();
  };
  cleanup(() => ws.close());
  return card({ title: t('pages.dashboard.just_now'), sub: t('pages.dashboard.events_sub'), actions: button({ size: 'sm', variant: 'ghost', href: '/events' }, t('pages.dashboard.all_events')), body });
}

export function dashboardPage({ el, signal, cleanup, every }) {
  const slot = h('div');
  el.append(pageHeader(t('nav.dashboard'), t('pages.dashboard.sub')), slot, eventsCard({ cleanup }));
  const reload = loadInto(slot, {
    signal,
    skeleton: () => h('div', { class: 'stack' }, sk.tiles(4), sk.block(220)),
    load: async (s) => {
      const [info, df, images, volumes, networks] = await Promise.all([
        get(base('/info'), { signal: s }), get(base('/df'), { signal: s }), get(base('/images'), { signal: s }),
        get(base('/volumes'), { signal: s }), get(base('/networks'), { signal: s })]);
      return { info, df, images, volumes, networks };
    },
    draw: overview,
  });
  every(15000, reload);
}

export { containerName };
