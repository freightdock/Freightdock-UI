// The cluster's network, live ("Cluster" 16, G9.8): outside the cluster on the left, then a column per node with the
// workloads placed on it, and a line for each pair of ends that talked since the last heartbeat, as thick as its
// traffic and moving the way it went. Pointing at (or focusing) a workload or a node shows only its lines; a click keeps
// that. Under it the same flows as a table, with their ports and bytes. Asked of the managers every few seconds; each
// connection was counted by one node only (the one it started on or came in on), so nothing is drawn twice.

import { h, s, mount, bytes, num } from '../dom.js';
import { get } from '../api.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { t } from '../i18n.js';
import { nodeName } from './cluster.js';

const EVERY_MS = 3000;
const COL_W = 210, COL_GAP = 120, PILL_H = 26, PILL_GAP = 8, HEAD_H = 34, PAD = 12, LOOP = 80, DASH_MS = 1200;

/** The key of a flow's end: what the picture draws it as. */
function endKey(e) {
  if (!e || e.kind === 'outside') return 'outside';
  if (e.kind === 'workload') return `w:${e.id}`;
  if (e.kind === 'node') return `n:${e.id}`;
  return `u:${e.address || ''}`;
}

const rate = (bps) => t('pages.network.per_second', { n: bytes(bps || 0) });

/** The flows added up per pair of ends: their bytes both ways, connections and ports. */
export function links(flows) {
  const by = new Map();
  for (const f of flows || []) {
    const from = endKey(f.from), to = endKey(f.to);
    const k = `${from}>${to}`;
    const l = by.get(k) || { from, to, out: 0, in: 0, connections: 0, ports: new Set() };
    l.out += f.out_bps || 0; l.in += f.in_bps || 0; l.connections += f.connections || 0;
    l.ports.add(f.published_port ? `${f.published_port}>${f.port}` : String(f.port));
    by.set(k, l);
  }
  return [...by.values()];
}

/** A line's width for its bytes per second: thin for a trickle, at most 10 for a flood. */
export const widthFor = (bps) => Math.min(10, 1.5 + 2 * Math.log10(1 + (bps || 0) / 1000));

export function clusterNetworkPage({ el, signal }) {
  const head = h('div'), body = h('div');
  el.append(head, body);
  mount(head, pageHeader(t('pages.network.title'), t('pages.network.sub')));
  // The end kept in focus by a click, and the one pointed at: both outlive a redraw.
  const view = { focus: null, hover: null };
  const reload = loadInto(body, {
    signal,
    load: async (sg) => {
      const [c, n, tr] = await Promise.all([get('/api/cluster', { signal: sg }), get('/api/cluster/nodes', { signal: sg }).catch(() => []), get('/api/cluster/traffic', { signal: sg }).catch(() => null)]);
      return { c, n, tr };
    },
    draw: ({ c, n, tr }) => {
      if (!c.connected) return emptyState(t('pages.cluster.not_connected'), t('pages.cluster.not_connected_text'));
      if (!tr) return emptyState(t('pages.cluster.no_answer'), '');
      if (!(tr.reporting || []).length) return emptyState(t('pages.network.none_reporting'), t('pages.network.none_reporting_text'));
      const nodes = [...(n || [])].sort((a, b) => nodeName(a).localeCompare(nodeName(b)));
      const ls = links(tr.flows);
      const pic = picture(nodes, tr.workloads || [], ls, view, (k) => { view.focus = view.focus === k ? null : k; reload(); });
      return h('div', { class: 'stack' },
        card({ title: t('pages.network.map'), sub: t('pages.network.map_sub'), body: pic }),
        card({ title: t('pages.network.flows'), body: flowTable(tr.flows || [], nodes, tr.workloads || []) }));
    },
  });
  const timer = setInterval(() => { if (!document.hidden) reload(); }, EVERY_MS);
  signal.addEventListener('abort', () => clearInterval(timer));
}

/** The picture: where each end sits, then the lines between them under the boxes. */
function picture(nodes, workloads, ls, view, toggle) {
  const where = new Map(); // end key -> { x, y, col }
  const boxes = [];
  const shapes = [];
  // Outside, on the left.
  where.set('outside', { x: PAD, y: PAD + HEAD_H, col: 0, w: COL_W });
  boxes.push({ key: 'outside', x: PAD, y: PAD, w: COL_W, h: HEAD_H + PILL_H + PAD, title: t('pages.network.outside'), pills: [] });
  let height = PAD * 2 + HEAD_H + PILL_H + PAD;
  nodes.forEach((n, i) => {
    const x = PAD + (i + 1) * (COL_W + COL_GAP);
    const mine = workloads.filter((w) => w.node_id === n.id).sort((a, b) => a.id.localeCompare(b.id));
    const pills = mine.map((w, j) => {
      const y = PAD + HEAD_H + j * (PILL_H + PILL_GAP);
      where.set(`w:${w.id}`, { x: x + PAD, y, col: i + 1, w: COL_W - 2 * PAD });
      return { key: `w:${w.id}`, x: x + PAD, y, label: w.id, sub: w.address || '' };
    });
    where.set(`n:${n.id}`, { x, y: PAD, col: i + 1, w: COL_W, node: true });
    const hgt = HEAD_H + Math.max(1, pills.length) * (PILL_H + PILL_GAP) + PAD;
    boxes.push({ key: `n:${n.id}`, x, y: PAD, w: COL_W, h: hgt, title: nodeName(n), pills });
    height = Math.max(height, PAD * 2 + hgt);
  });
  // Room on the right for the last column's lines that loop back into it.
  const width = PAD * 2 + (nodes.length + 1) * COL_W + nodes.length * COL_GAP + LOOP;

  const related = (k) => new Set(ls.filter((l) => l.from === k || l.to === k).flatMap((l) => [l.from, l.to]));
  const phase = -(performance.now() % DASH_MS);
  const lines = ls.map((l) => {
    const a = where.get(l.from), b = where.get(l.to);
    if (!a || !b) return null;
    const y1 = a.y + (a.node ? HEAD_H / 2 : PILL_H / 2), y2 = b.y + (b.node ? HEAD_H / 2 : PILL_H / 2);
    let d;
    if (a.col === b.col) {
      // On one node: out of the right edge and back in.
      const x = a.x + a.w, bend = Math.min(LOOP - 10, 30 + Math.abs(y2 - y1) / 4);
      d = `M ${x} ${y1} C ${x + bend} ${y1}, ${x + bend} ${y2}, ${x} ${y2}`;
    } else {
      const right = a.col < b.col;
      const x1 = right ? a.x + a.w : a.x, x2 = right ? b.x : b.x + b.w;
      const mid = (x1 + x2) / 2;
      d = `M ${x1} ${y1} C ${mid} ${y1}, ${mid} ${y2}, ${x2} ${y2}`;
    }
    const total = l.out + l.in;
    const outside = l.from === 'outside' || l.to === 'outside';
    return s('path', { d, class: `fd-net__flow${outside ? ' fd-net__flow--outside' : ''}`, 'stroke-width': widthFor(total).toFixed(1), style: { animationDelay: `${phase}ms` }, 'data-from': l.from, 'data-to': l.to },
      s('title', null, t('pages.network.line_title', { out: rate(l.out), in: rate(l.in), ports: [...l.ports].join(', '), n: num(l.connections) })));
  }).filter(Boolean);

  const svg = s('svg', { class: 'fd-net', viewBox: `0 0 ${width} ${height}`, width, height, role: 'img', 'aria-label': t('pages.network.map_label', { n: ls.length }) });
  const show = (k) => {
    const keep = k ? related(k) : null;
    svg.classList.toggle('is-focus', !!k);
    for (const p of svg.querySelectorAll('.fd-net__flow')) p.classList.toggle('is-on', !!k && (p.dataset.from === k || p.dataset.to === k));
    for (const g of svg.querySelectorAll('[data-key]')) g.classList.toggle('is-on', !!keep && keep.has(g.dataset.key));
  };
  const end = (key, x, y, w, hh, cls, text, sub) => s('g', {
    class: `fd-net__end ${cls}`, 'data-key': key, tabindex: '0', role: 'button', 'aria-label': text,
    onMouseenter: () => { view.hover = key; show(key); }, onMouseleave: () => { view.hover = null; show(view.focus); },
    onFocus: () => show(key), onBlur: () => show(view.hover || view.focus),
    onClick: () => toggle(key), onKeydown: (e) => { if (e.key === 'Enter' || e.key === ' ') { e.preventDefault(); toggle(key); } },
  },
  s('rect', { x, y, width: w, height: hh, rx: 6 }),
  s('text', { x: x + 10, y: y + hh / 2 + 4, class: 'fd-net__label' }, text.length > 26 ? `${text.slice(0, 25)}…` : text),
  s('title', null, sub ? `${text} (${sub})` : text));

  for (const b of boxes) {
    shapes.push(s('rect', { class: 'fd-net__box', x: b.x, y: b.y, width: b.w, height: b.h, rx: 10 }));
    shapes.push(b.key === 'outside'
      ? end('outside', b.x + PAD, b.y + HEAD_H, b.w - 2 * PAD, PILL_H, 'fd-net__end--outside', t('pages.network.outside_clients'), '')
      : end(b.key, b.x, b.y, b.w, HEAD_H, 'fd-net__end--node', b.title, ''));
    for (const p of b.pills) shapes.push(end(p.key, p.x, p.y, COL_W - 2 * PAD, PILL_H, 'fd-net__end--workload', p.label, p.sub));
    if (b.key !== 'outside' && !b.pills.length) shapes.push(s('text', { x: b.x + PAD, y: b.y + HEAD_H + PILL_H / 2 + 4, class: 'fd-net__sub' }, t('pages.network.no_workloads')));
  }
  svg.append(s('g', { class: 'fd-net__boxes' }, shapes.filter((x) => x.classList.contains('fd-net__box'))), s('g', { class: 'fd-net__flows' }, lines), s('g', null, shapes.filter((x) => !x.classList.contains('fd-net__box'))));
  show(view.hover || view.focus);
  return h('div', { class: 'fd-net-wrap' }, svg,
    ls.length ? null : h('p', { class: 'sub' }, t('pages.network.quiet')));
}

/** The flows one by one, as each node counted them. */
function flowTable(flows, nodes, workloads) {
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const wl = new Map(workloads.map((w) => [w.id, w]));
  const label = (e) => {
    if (!e || e.kind === 'outside') return t('pages.network.outside');
    if (e.kind === 'workload') return e.id;
    if (e.kind === 'node') return byId.get(e.id) ? nodeName(byId.get(e.id)) : e.id;
    return e.address || '';
  };
  const where = (e) => (e && e.kind === 'workload' && wl.get(e.id) && byId.get(wl.get(e.id).node_id) ? nodeName(byId.get(wl.get(e.id).node_id)) : '');
  return table({
    rows: flows, label: t('pages.network.flows'), empty: [t('pages.network.quiet'), ''],
    columns: [
      { label: t('pages.network.from'), cell: (f) => h('div', { class: 'name-cell' }, h('span', { class: 'mono' }, label(f.from)), h('span', { class: 'sub' }, where(f.from))), sort: (f) => label(f.from) },
      { label: t('pages.network.to'), cell: (f) => h('div', { class: 'name-cell' }, h('span', { class: 'mono' }, label(f.to)), h('span', { class: 'sub' }, where(f.to))), sort: (f) => label(f.to) },
      { label: t('pages.network.port'), cell: (f) => h('span', { class: 'mono nowrap' }, f.published_port ? t('pages.network.port_via', { port: String(f.port), via: String(f.published_port), protocol: f.protocol }) : `${f.port}/${f.protocol}`), sort: (f) => f.port },
      { label: t('pages.network.out'), cell: (f) => h('span', { class: 'nowrap' }, rate(f.out_bps)), sort: (f) => f.out_bps || 0 },
      { label: t('pages.network.in'), cell: (f) => h('span', { class: 'nowrap' }, rate(f.in_bps)), sort: (f) => f.in_bps || 0 },
      { label: t('pages.network.connections'), cell: (f) => num(f.connections || 0), sort: (f) => f.connections || 0 },
      { label: t('pages.network.counted_by'), cell: (f) => (byId.get(f.node) ? nodeName(byId.get(f.node)) : f.node), sort: (f) => f.node },
    ],
  });
}
