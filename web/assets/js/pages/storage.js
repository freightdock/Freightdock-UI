// The Storage page ("Cluster" 43): what the cluster's disks hold, by stack. Two lists, each under its totals (size,
// free, used, share used): the workers, each with its own disk, and the shared storages (Ceph, NFS, SMB, a shared
// directory). Pressing one opens the stacks on it, each with what it takes and its share of the whole; a worker's
// also says what is no stack's (images, volumes no stack mounts, the build cache, the rest of the system), a storage's
// what each stack keeps in which folder. The workers measure every few minutes, so the numbers are as old as their
// last measure, which is said; one nobody measured is said so, never shown as nothing.

import { h, mount, bytes, num, ago } from '../dom.js';
import { get } from '../api.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { meter } from '../../finui/components/meter/meter.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { t, tOr } from '../i18n.js';
import { totals, share } from '../usage-sums.js';

const EVERY_MS = 30000;
const HEALTH_TONES = { healthy: 'good', suspect: 'warning', unreachable: 'critical' };

// A share as a percentage; one above nothing but under a tenth of a percent is said so, never "0.0 %".
const pct = (x) => (x == null ? '' : x > 0 && x < 0.001 ? t('pages.storage.tiny') : t('units.percent', { n: num(x * 100, x < 0.1 ? 1 : 0) }));
/** The tone of a disk this full: warning from 80 %, critical from 90 %. */
const fullTone = (x) => (x == null ? 'info' : x >= 0.9 ? 'critical' : x >= 0.8 ? 'warning' : 'good');

export function storagePage({ el, signal }) {
  const head = h('div'), body = h('div');
  el.append(head, body);
  mount(head, pageHeader(t('pages.storage.title'), t('pages.storage.sub')));
  // What is open stays open across the refreshes.
  const open = new Set();
  const reload = loadInto(body, {
    signal,
    load: (sg) => get('/api/cluster/usage', { signal: sg }),
    draw: (u) => h('div', { class: 'stack' },
      section('node', t('pages.storage.workers'), t('pages.storage.workers_sub'), u.nodes || [], open, nodeRow, nodeDetail),
      section('storage', t('pages.storage.shared'), t('pages.storage.shared_sub'), u.storage || [], open, storageRow, storageDetail)),
  });
  const timer = setInterval(() => { if (!document.hidden) reload(); }, EVERY_MS);
  signal.addEventListener('abort', () => clearInterval(timer));
}

/** The totals over `list`: size, free, used and the share used. */
function sums(list) {
  const s = totals(list);
  const tile = (label, value, extra) => h('div', { class: 'fd-store__sum' }, h('span', { class: 'sub' }, label), h('strong', { class: 'fd-store__num' }, value), extra || null);
  return h('div', { class: 'stack' },
    h('div', { class: 'fd-store__sums' },
      tile(t('pages.storage.total'), bytes(s.total)),
      tile(t('pages.storage.free'), bytes(s.free)),
      tile(t('pages.storage.used'), bytes(s.used)),
      tile(t('pages.storage.used_share'), s.share == null ? '-' : pct(s.share), meter({ value: s.share || 0, block: true, label: t('pages.storage.used_share') }))),
    s.unknown ? h('p', { class: 'sub' }, t('pages.storage.unknown_n', { count: s.unknown })) : null);
}

/** A card with the totals over `list` and a row for each, pressed to open its stacks. */
function section(kind, title, sub, list, open, rowOf, detailOf) {
  if (!list.length) return card({ title, sub, body: emptyState(t(`pages.storage.none_${kind}`), '') });
  const rows = list.map((x) => {
    const key = `${kind}:${x.id || x.name}`;
    const detail = h('div', { class: 'fd-store__detail', hidden: !open.has(key) }, open.has(key) ? detailOf(x) : null);
    const btn = h('button', { type: 'button', class: 'fd-store__row', 'aria-expanded': String(open.has(key)), onClick: () => {
      const on = !open.has(key);
      if (on) open.add(key); else open.delete(key);
      btn.setAttribute('aria-expanded', String(on));
      detail.hidden = !on;
      mount(detail, on ? detailOf(x) : null);
    } }, rowOf(x));
    return h('div', { class: 'fd-store__item' }, btn, detail);
  });
  return card({ title, sub, body: h('div', { class: 'stack' }, sums(list), h('div', { class: 'fd-store__list' }, rows)) });
}

/** Used of total and the share, as a bar. */
function fill(x) {
  const s = share(x.used_bytes, x.total_bytes);
  return h('div', { class: 'fd-dash-bar' },
    meter({ value: s || 0, block: true, label: s == null ? t('pages.storage.no_size') : pct(s) }),
    h('span', { class: 'sub nowrap' }, s == null ? t('pages.storage.no_size') : t('pages.storage.used_of', { used: bytes(x.used_bytes), total: bytes(x.total_bytes), pct: pct(s) })));
}

function nodeRow(n) {
  const hl = n.health || 'unreachable';
  return [
    h('div', { class: 'fd-store__name' }, h('strong', null, n.name), status({ tone: HEALTH_TONES[hl] || 'info', line: true }, tOr(`pages.cluster.health_word.${hl}`, hl))),
    fill(n),
    h('span', { class: 'sub fd-store__count' }, t('pages.storage.stacks_n', { count: (n.stacks || []).length }))];
}

function storageRow(st) {
  const s = share(st.used_bytes, st.total_bytes);
  return [
    h('div', { class: 'fd-store__name' }, h('strong', null, st.name), h('span', { class: 'sub' }, st.driver),
      s != null ? status({ tone: fullTone(s), line: true }, pct(s)) : null),
    fill(st),
    h('span', { class: 'sub fd-store__count' }, t('pages.storage.stacks_n', { count: (st.stacks || []).length }))];
}

/** When it was measured, or that it was not. */
function measured(at, by) {
  if (!at) return h('p', { class: 'sub' }, t('pages.storage.not_measured'));
  return h('p', { class: 'sub' }, by ? t('pages.storage.measured_by', { when: ago(at / 1000), by }) : t('pages.storage.measured', { when: ago(at / 1000) }));
}

/** The stacks' table: each with what it takes and its share of `total`; then what is no stack's, `rest`. */
function stackTable(stacks, total, rest, extraCol) {
  const rows = [...stacks.map((x) => ({ ...x, kind: 'stack' })), ...rest.filter((r) => r.bytes).map((r) => ({ ...r, kind: 'rest' }))];
  return table({
    rows, filter: false, dense: true, label: t('pages.storage.stacks'),
    columns: [
      { label: t('pages.storage.stack'), cell: (r) => (r.kind === 'stack' ? h('span', { class: 'mono' }, r.stack) : h('span', { class: 'sub' }, r.label)), sort: (r) => r.stack || r.label },
      ...(extraCol ? [extraCol] : []),
      { label: t('pages.storage.used'), cell: (r) => (r.bytes == null ? h('span', { class: 'sub' }, t('pages.storage.not_measured_short')) : h('span', { class: 'nowrap' }, `${r.partial || r.measured === false ? '≥ ' : ''}${bytes(r.bytes)}`)), sort: (r) => r.bytes || 0 },
      { label: t('pages.storage.share_of_total'), cell: (r) => {
        const s = share(r.bytes, total);
        return s == null ? '' : h('div', { class: 'fd-dash-bar fd-store__share' }, meter({ value: s, block: true, label: pct(s) }), h('span', { class: 'sub nowrap' }, pct(s)));
      }, sort: (r) => r.bytes || 0 },
    ],
  });
}

function nodeDetail(n) {
  const stacks = n.stacks || [];
  const rest = [
    { label: t('pages.storage.images'), bytes: n.images_bytes },
    { label: t('pages.storage.loose_volumes'), bytes: n.loose_volumes_bytes },
    { label: t('pages.storage.build_cache'), bytes: n.build_cache_bytes },
    { label: t('pages.storage.other'), bytes: n.other_bytes },
  ];
  return h('div', { class: 'stack' },
    measured(n.measured_at),
    n.measured_at ? stackTable(stacks, n.total_bytes, rest, { label: t('pages.storage.split'), cell: (r) => (r.kind === 'stack' ? h('span', { class: 'sub nowrap' }, t('pages.storage.split_text', { containers: bytes(r.containers_bytes), volumes: bytes(r.volumes_bytes) })) : '') }) : null,
    n.measured_at && !stacks.length ? h('p', { class: 'sub' }, t('pages.storage.no_stacks_here')) : null);
}

function storageDetail(st) {
  const stacks = st.stacks || [];
  const folders = { label: t('pages.storage.folders'), cell: (r) => (r.kind === 'stack' ? h('div', { class: 'fd-store__folders' }, (r.volumes || []).map((v) =>
    h('span', { class: 'sub mono', title: v.measured_by ? t('pages.storage.folder_measured', { when: ago(v.measured_at / 1000), by: v.measured_by }) : t('pages.storage.folder_not_measured') },
      `/${v.dir}${v.bytes == null ? ` (${t('pages.storage.not_measured_short')})` : ''}`))) : '') };
  return h('div', { class: 'stack' },
    measured(st.probed_at, st.probed_by),
    stacks.length ? stackTable(stacks, st.total_bytes, [{ label: t('pages.storage.not_stacks'), bytes: st.other_bytes }], folders) : h('p', { class: 'sub' }, t('pages.storage.no_stacks_on')),
    stacks.some((x) => x.bytes == null || x.measured === false) ? h('p', { class: 'sub' }, t('pages.storage.why_not_measured')) : null);
}
