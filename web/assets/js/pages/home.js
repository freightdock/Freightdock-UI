// Home: every host Freightdock manages, one row each, and the way into one. It is drawn at once from what Freightdock keeps
// (health, group, tags, and each host's last known numbers), then each row fills with its host's numbers as that host
// answers: a slow host or one that is down never holds the page up, and a host that is down keeps its last numbers,
// greyed, with when they are from. Above the list, the hosts counted (each count a quick filter); a toolbar sorts,
// filters by group and tag, and searches; at the foot, pages of 10, 25 or 50.

import { h, mount, bytes, ago, when, num } from '../dom.js';
import { get, isAbort } from '../api.js';
import { state, isAdmin, setEnv, setEnvs } from '../state.js';
import { roleName } from '../grants.js';
import { envBadge } from '../docker.js';
import { filterBox, matches } from '../view.js';
import { editDialog } from './environments.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { chip } from '../../finui/components/chip/chip.js';
import { pagination } from '../../finui/components/pagination/pagination.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { errorState } from '../../finui/components/error/error.js';
import { status } from '../../finui/components/badge/badge.js';
import { sk } from '../../finui/components/skeleton/skeleton.js';
import { icon } from '../../finui/core.js';
import { t, lang } from '../i18n.js';

const KIND = { local: t('pages.home.kind.local'), agent: t('pages.home.kind.agent'), tls: t('pages.home.kind.tls') };
const KIND_ORDER = { local: 0, agent: 1, tls: 2 };
const HEALTH_ORDER = { down: 0, unknown: 1, up: 2 };
const PER_PAGE = [10, 25, 50];
const QUICK = [
  { key: 'all', label: t('pages.home.hosts') },
  { key: 'up', label: t('pages.home.up'), tone: 'good' },
  { key: 'down', label: t('pages.home.down'), tone: 'critical' },
  { key: 'outdated', label: t('pages.home.outdated'), tone: 'warning' },
  { key: 'unassigned', label: t('pages.home.in_no_group') },
];

const outdated = (e) => !!(e.agent && e.agent.outdated);

/** How many hosts there are of each quick filter. */
export function counts(envs) {
  return {
    all: envs.length,
    up: envs.filter((e) => e.status === 'up').length,
    down: envs.filter((e) => e.status === 'down').length,
    outdated: envs.filter(outdated).length,
    unassigned: envs.filter((e) => !e.group).length,
  };
}

/** The hosts a quick filter, a group ('' any, '-' none), a tag and the search words leave. */
export function pick(envs, { quick = 'all', group = '', tag = '', words = [] }) {
  return envs.filter((e) => {
    if (quick === 'up' && e.status !== 'up') return false;
    if (quick === 'down' && e.status !== 'down') return false;
    if (quick === 'outdated' && !outdated(e)) return false;
    if (quick === 'unassigned' && e.group) return false;
    if (group === '-' && e.group) return false;
    if (group && group !== '-' && (!e.group || String(e.group.id) !== group)) return false;
    if (tag && !e.tags.some((x) => x.toLowerCase() === tag.toLowerCase())) return false;
    return matches(words, e.name, e.url, KIND[e.kind], e.docker_version, e.group && e.group.name, ...e.tags,
      e.agent && (e.agent.peer || e.agent.last_peer));
  });
}

/** The hosts in the order asked: by age (as added), group (none last), kind, or health (down first); then by name. */
export function order(envs, by) {
  const name = (a, b) => a.name.localeCompare(b.name, lang());
  const key = {
    age: (a, b) => a.id - b.id,
    group: (a, b) => (a.group ? 0 : 1) - (b.group ? 0 : 1) || (a.group && b.group ? a.group.name.localeCompare(b.group.name, lang()) : 0) || name(a, b),
    kind: (a, b) => (KIND_ORDER[a.kind] ?? 9) - (KIND_ORDER[b.kind] ?? 9) || name(a, b),
    health: (a, b) => (HEALTH_ORDER[a.status] ?? 1) - (HEALTH_ORDER[b.status] ?? 1) || name(a, b),
  }[by] || ((a, b) => a.id - b.id);
  return [...envs].sort(key);
}

/** Where a host is, as people read it. */
function address(e) {
  if (e.kind !== 'agent') return e.url;
  const a = e.agent || {};
  if (a.peer) return t('pages.home.agent_from', { peer: a.peer });
  if (a.last_peer) return t('pages.home.agent_last_from', { peer: a.last_peer });
  return a.enrolled ? t('pages.home.agent_not_connected') : t('pages.home.agent_not_started');
}

const keep = { read: (k) => { try { return localStorage.getItem('freightdock.' + k); } catch { return null; } },
  write: (k, v) => { try { localStorage.setItem('freightdock.' + k, v); } catch { /* not kept */ } } };

const fig = (name, value, label, title) => h('div', { class: 'home-fig', title: title || null },
  icon(name, 16), h('div', { class: 'home-fig__text' }, h('strong', { class: 'home-fig__value' }, value), h('span', { class: 'home-fig__label' }, label)));

export function homePage({ el, signal, every }) {
  let envs = null, failed = null;
  const live = new Map();       // env id -> { data, at } from the host just now, or { error }
  const asking = new Set();     // env ids whose answer is on its way
  const painters = new Map();   // env id -> repaint of its row's numbers, for the rows drawn now
  const f = { quick: 'all', group: '', tag: '', words: [], sort: keep.read('homeSort') || 'age', page: 1,
    perPage: PER_PAGE.includes(Number(keep.read('homePerPage'))) ? Number(keep.read('homePerPage')) : 10 };

  // ---- the counts, each a quick filter
  const quickButtons = QUICK.map((q) => {
    const n = h('span', { class: 'home-count__n' }, '–');
    const b = h('button', { type: 'button', class: ['home-count', q.tone && `home-count--${q.tone}`], 'aria-pressed': 'false',
      onClick: () => { f.quick = f.quick === q.key && q.key !== 'all' ? 'all' : q.key; f.page = 1; draw(); } },
    q.tone ? h('span', { class: 'home-count__dot', 'aria-hidden': 'true' }) : null, n, h('span', { class: 'home-count__label' }, q.label));
    return { q, b, n };
  });
  const strip = h('div', { class: 'home-counts', role: 'group', 'aria-label': t('pages.home.by_state') }, quickButtons.map((x) => x.b));

  // ---- the toolbar: sort, group, tag, search; it lives outside what is redrawn ("UI" 5)
  const sort = segmented({ label: t('pages.home.sort_by'), size: 'sm', value: f.sort, onChange: (v) => { f.sort = v; keep.write('homeSort', v); draw(); },
    options: [{ value: 'age', label: t('pages.home.age') }, { value: 'group', label: t('pages.home.group') }, { value: 'kind', label: t('pages.networks.kind') }, { value: 'health', label: t('pages.home.health') }] });
  const groupSel = h('select', { class: 'fui-field__input home-select', 'aria-label': t('pages.home.group') });
  const tagSel = h('select', { class: 'fui-field__input home-select', 'aria-label': t('common.tag') });
  groupSel.addEventListener('change', () => { f.group = groupSel.value; f.page = 1; draw(); });
  tagSel.addEventListener('change', () => { f.tag = tagSel.value; f.page = 1; draw(); });
  const search = filterBox(t('pages.home.search'), (w) => { f.words = w; f.page = 1; draw(); });
  const toolbar = h('div', { class: 'toolbar home-toolbar' }, h('span', { class: 'home-toolbar__label faint' }, t('pages.home.sort_by')), sort, groupSel, tagSel, search);

  const list = h('div', { class: 'home-list' }, sk.tableRows(4));
  const foot = h('div', { class: 'home-foot' });

  function fillSelects() {
    const groups = new Map(), tags = new Map();
    for (const e of envs) {
      if (e.group) groups.set(String(e.group.id), e.group.name);
      for (const tag of e.tags) tags.set(tag.toLowerCase(), tag);
    }
    if (f.group && f.group !== '-' && !groups.has(f.group)) f.group = '';
    if (f.tag && !tags.has(f.tag.toLowerCase())) f.tag = '';
    mount(groupSel, h('option', { value: '' }, t('pages.home.every_group')), h('option', { value: '-', selected: f.group === '-' }, t('pages.home.in_no_group')),
      [...groups].sort((a, b) => a[1].localeCompare(b[1], lang())).map(([id, n]) => h('option', { value: id, selected: f.group === id }, n)));
    mount(tagSel, h('option', { value: '' }, t('pages.home.every_tag')),
      [...tags.values()].sort((a, b) => a.localeCompare(b, lang())).map((tag) => h('option', { value: tag, selected: f.tag.toLowerCase() === tag.toLowerCase() }, tag)));
    groupSel.hidden = !groups.size;
    tagSel.hidden = !tags.size;
  }

  function open(e) { setEnv(e.id); }

  /** A host's numbers: from it just now, else the last it gave, greyed. */
  function numbers(e) {
    const now = live.get(e.id);
    const fresh = now && now.data && e.status !== 'down' ? now.data : null;
    const s = fresh || e.snapshot;
    const box = h('div', { class: ['home-figs', !fresh && s && 'is-stale'] });
    if (!s) {
      const waiting = asking.has(e.id) && e.status !== 'down';
      return { box: mount(box, waiting ? [0, 1, 2, 3].map(() => sk.line('70%', 28)) : h('span', { class: 'faint home-figs__none' }, t('pages.home.no_numbers'))), extra: null };
    }
    const c = s.containers || {};
    mount(box,
      fig('package', `${num(c.running ?? 0)} / ${num(c.total ?? 0)}`, t('pages.home.running'), c.paused ? t('pages.home.running_title_paused', { running: c.running ?? 0, stopped: c.stopped ?? 0, paused: c.paused }) : t('pages.home.running_title', { running: c.running ?? 0, stopped: c.stopped ?? 0 })),
      fig('server', num(s.nodes ?? 1), t('pages.home.node_word', { count: s.nodes ?? 1 }), s.swarm ? t('pages.home.swarm_nodes') : t('pages.home.alone')),
      fig('cpu', num(s.cpus ?? 0), t('pages.home.core_word', { count: s.cpus ?? 0 })),
      fig('hardDrive', bytes(s.memory || 0), t('pages.home.memory')));
    const parts = [t('common.n_images', { count: s.images ?? 0 }), t('common.n_volumes', { count: s.volumes ?? 0 }), s.stacks ? t('common.n_stacks', { count: s.stacks }) : null].filter(Boolean);
    const extra = [
      h('span', null, parts.join(' · ')),
      c.unhealthy ? status({ tone: 'warning' }, t('pages.home.unhealthy', { count: c.unhealthy })) : null,
      !fresh ? h('span', { class: 'faint', title: when(s.at) }, t('pages.home.as_last_seen', { when: ago(s.at) })) : null,
    ];
    return { box, extra };
  }

  function row(e) {
    const a = e.agent || {};
    const figsSlot = h('div', { class: 'home-row__figs' });
    const extraSlot = h('div', { class: 'home-row__line home-row__extra' });
    const paint = () => { const n = numbers(e); mount(figsSlot, n.box); mount(extraSlot, n.extra); };
    paint();
    painters.set(e.id, paint);
    const version = e.docker_version ? `${(e.snapshot && e.snapshot.swarm) ? 'Swarm' : 'Docker'} ${e.docker_version}` : null;
    const seen = e.last_seen_at ? h('span', { class: 'nowrap', title: when(e.last_seen_at) }, icon('clock', 13), ' ' + t(e.status === 'down' ? 'pages.home.last_seen' : 'pages.home.seen', { when: ago(e.last_seen_at) })) : h('span', { class: 'faint' }, t('pages.home.never_seen'));
    const kind = e.kind === 'agent' && a.version
      ? h('span', { class: ['nowrap', a.outdated && 'home-row__old'], title: a.outdated ? t('pages.home.older', { version: (state.status && state.status.version) || '' }) : null },
        a.outdated ? icon('alert', 13) : null, ' ' + t('pages.home.agent_version', { version: a.version }))
      : h('span', null, KIND[e.kind] || e.kind);
    const main = h('a', { class: 'home-row__main', href: '/', onClick: () => open(e), 'aria-label': t('pages.home.open', { name: e.name }) },
      h('span', { class: 'home-row__icon', 'aria-hidden': 'true' }, icon(e.kind === 'agent' ? 'plug' : e.kind === 'tls' ? 'lock' : 'server', 20)),
      h('div', { class: 'home-row__info' },
        h('div', { class: 'home-row__line home-row__head' }, h('span', { class: 'home-row__name' }, e.name), h('span', { title: e.last_error || '' }, envBadge(e)),
          e.id === state.env ? h('span', { class: 'fui-chip' }, t('pages.home.chosen')) : null),
        h('div', { class: 'home-row__line faint' }, seen, version ? h('span', { class: 'nowrap' }, version) : null, h('span', { class: 'mono truncate' }, address(e))),
        h('div', { class: 'home-row__line' }, h('span', { class: 'faint' }, e.group ? t('pages.home.group_of', { name: e.group.name }) : t('pages.home.in_no_group')), kind,
          isAdmin() ? null : h('span', { class: 'faint' }, t('pages.home.you', { role: roleName(e.role) })),
          e.tags.length ? h('span', { class: 'home-row__tags' }, e.tags.map((tag) => chip(tag))) : null),
        extraSlot),
      figsSlot);
    const actions = isAdmin() ? h('div', { class: 'home-row__actions' },
      button({ variant: 'icon', size: 'sm', 'aria-label': t('routes.edit', { name: e.name }), title: t('pages.home.edit_title'), onClick: () => editDialog(e, reload) }, icon('pencil', 15)),
      button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.home.settings_of', { name: e.name }), title: t('pages.home.settings_title'), href: '/environments' }, icon('settings', 15))) : null;
    return h('li', { class: ['home-row', e.status === 'down' && 'is-down'] }, main, actions);
  }

  function draw() {
    painters.clear();
    for (const { q, b, n } of quickButtons) { b.setAttribute('aria-pressed', String(f.quick === q.key)); n.textContent = envs ? String(counts(envs)[q.key]) : '–'; }
    if (failed) { mount(list, errorState(failed, reload)); mount(foot); return; }
    if (!envs) return;
    const shown = order(pick(envs, f), f.sort);
    const pages = Math.max(1, Math.ceil(shown.length / f.perPage));
    f.page = Math.min(f.page, pages);
    const page = shown.slice((f.page - 1) * f.perPage, f.page * f.perPage);
    mount(list, page.length ? h('ul', { class: 'home-rows' }, page.map(row))
      : envs.length ? emptyState(t('pages.home.no_match'), t('pages.home.no_match_text')) : (isAdmin() ? emptyState(t('pages.home.none'), t('pages.home.none_text')) : emptyState(t('pages.home.none_for_you'), t('pages.home.none_for_you_text'))));
    const perPage = h('select', { class: 'fui-field__input home-select', 'aria-label': t('pages.home.per_page') }, PER_PAGE.map((n) => h('option', { value: String(n), selected: n === f.perPage }, t('pages.home.n_a_page', { n }))));
    perPage.addEventListener('change', () => { f.perPage = Number(perPage.value); keep.write('homePerPage', perPage.value); f.page = 1; draw(); });
    mount(foot, shown.length > PER_PAGE[0] ? [pagination({ page: f.page, perPage: f.perPage, total: shown.length, onPage: (p) => { f.page = p; draw(); } }), perPage] : null);
  }

  /** Asks each host for its numbers, each on its own; a row is drawn again when its answer comes. */
  function askAll() {
    for (const e of envs || []) {
      if (asking.has(e.id)) continue;
      asking.add(e.id);
      get(`/api/envs/${e.id}/summary`, { signal }).then((data) => {
        live.set(e.id, { data });
        const now = envs.find((x) => x.id === e.id);
        if (now) { now.status = 'up'; now.last_seen_at = data.at; now.snapshot = data; if (data.docker_version) now.docker_version = data.docker_version; }
      }, (err) => {
        if (isAbort(err)) return;
        live.set(e.id, { error: err.message });
      }).finally(() => {
        asking.delete(e.id);
        if (signal.aborted) return;
        // The counts may change with it (it answered, so it is up); its row is drawn again on its own.
        const p = painters.get(e.id);
        if (p && live.get(e.id) && live.get(e.id).error) p(); else draw();
      });
    }
  }

  async function reload() {
    try {
      const l = await get('/api/environments', { signal });
      if (signal.aborted) return;
      envs = l; failed = null;
      setEnvs(l);
      fillSelects();
      askAll();
    } catch (e) {
      if (isAbort(e) || signal.aborted) return;
      if (!envs) failed = e;
    }
    draw();
  }

  const refresh = button({ size: 'sm', variant: 'ghost', 'aria-label': t('pages.home.ask_again'), title: t('pages.home.ask_again'), onClick: () => reload() }, icon('refresh', 14));
  el.append(pageHeader(t('nav.home'), t('pages.home.sub'), refresh), strip,
    h('section', { class: 'fui-card fui-card--flush home-card' }, h('div', { class: 'home-card__bar' }, toolbar), list, foot));
  draw();
  reload();
  every(30000, reload);
}

