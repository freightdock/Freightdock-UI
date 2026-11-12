// Patch notes: every release, newest first, straight from CHANGELOG.md, as FinStats draws its own. Releases of one
// minor series stand together under that series' headline; every note is a row with its kind named before it.

import { h, icon } from '../dom.js';
import { get } from '../api.js';
import { markVersionSeen } from '../state.js';
import { loadInto } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { t, tOr, lang } from '../i18n.js';

const KINDS = {
  Added: { icon: 'plus', cls: 'cl-added' },
  Changed: { icon: 'sliders', cls: 'cl-changed' },
  Security: { icon: 'shield', cls: 'cl-security' },
  Performance: { icon: 'gauge', cls: 'cl-performance' },
  Stability: { icon: 'anchor', cls: 'cl-stability' },
  Fixed: { icon: 'check', cls: 'cl-fixed' },
  Removed: { icon: 'x', cls: 'cl-removed' },
};

/** Notes may use **bold**, *italic* and `code`. Built as DOM nodes; nothing is ever parsed as HTML. */
export function inline(text) {
  const out = [];
  const re = /\*\*([^*]+)\*\*|\*([^*\s][^*]*?)\*|`([^`]+)`/g;
  let last = 0, m;
  while ((m = re.exec(text))) {
    if (m.index > last) out.push(text.slice(last, m.index));
    out.push(m[1] != null ? h('strong', null, inline(m[1])) : m[2] != null ? h('em', null, inline(m[2])) : h('code', { class: 'mono cl-code' }, m[3]));
    last = re.lastIndex;
  }
  if (last < text.length) out.push(text.slice(last));
  return out;
}

const day = (iso) => (/^\d{4}-\d{2}-\d{2}$/.test(iso || '') ? new Date(iso + 'T12:00:00') : null);
/** "9 November 2026", or a span of days as the language writes one. */
function days(first, last) {
  const a = day(first), b = day(last);
  if (!b) return last || '';
  const f = new Intl.DateTimeFormat(lang(), { year: 'numeric', month: 'long', day: 'numeric' });
  return a && a.getTime() !== b.getTime() && f.formatRange ? f.formatRange(a, b) : f.format(b);
}

function notes(g) {
  const k = KINDS[g.kind] || KINDS.Changed;
  return h('section', { class: 'cl-group' },
    h('h3', { class: ['cl-kind', k.cls] }, icon(k.icon, 14), tOr(`pages.changelog.kind.${g.kind}`, g.kind)),
    h('ul', { class: 'cl-items' }, g.items.map((it) => h('li', null,
      h('span', { class: ['cl-tag', k.cls], 'aria-hidden': 'true' }, icon(k.icon, 14), h('span', { class: 'cl-tag-word' }, tOr(`pages.changelog.kind.${g.kind}`, g.kind))),
      h('span', { class: 'cl-text' }, inline(it))))));
}

function release(r, current, about) {
  const running = r.version === current;
  const summary = r.summary && r.summary.trim() !== about ? r.summary : null;
  return h('article', { class: ['cl-release', running && 'is-current'], 'aria-labelledby': `cl-${r.version}` },
    h('header', { class: 'cl-head' },
      h('h2', { class: 'cl-version mono', id: `cl-${r.version}` }, 'v' + r.version),
      running ? h('span', { class: 'fui-chip cl-running' }, icon('check', 12), t('pages.changelog.running')) : null,
      r.date ? h('time', { class: 'cl-date', dateTime: r.date }, days(r.started || r.date, r.date)) : null),
    summary ? h('p', { class: 'cl-summary' }, inline(summary)) : null,
    (r.groups || []).filter((g) => g.items && g.items.length).map(notes));
}

/** Releases of one minor series (0.7.0 ... 0.7.3) together, newest series first. */
function series(releases) {
  const out = [];
  for (const r of releases) {
    const key = String(r.version).split('.').slice(0, 2).join('.');
    const last = out[out.length - 1];
    if (last && last.key === key) last.releases.push(r); else out.push({ key, releases: [r] });
  }
  return out;
}

function seriesSection(g, current) {
  const newest = g.releases[0], oldest = g.releases[g.releases.length - 1];
  const about = (oldest.summary || newest.summary || '').trim();
  const headline = (about.match(/^.+?[.!?](?=\s|$)/) || [about])[0];
  const n = g.releases.length;
  return h('section', { class: ['cl-series', g.releases.some((r) => r.version === current) && 'is-current'] },
    h('div', { class: 'cl-series-head' },
      h('span', { class: 'cl-series-name mono' }, `v${g.key}`),
      h('span', { class: 'cl-series-range mono' }, n > 1 ? t('pages.changelog.range', { from: oldest.version, to: newest.version }) : newest.version),
      headline ? h('span', { class: 'cl-series-about' }, inline(headline)) : null,
      h('span', { class: 'cl-series-meta' }, `${t('pages.changelog.releases', { count: n })} · ${days(oldest.started || oldest.date, newest.date)}`)),
    h('div', { class: 'cl-series-body' }, g.releases.map((r) => release(r, current, about))));
}

export function changelogPage({ el, signal }) {
  const slot = h('div');
  el.append(pageHeader(t('nav.patch_notes'), t('pages.changelog.sub')), slot);
  loadInto(slot, {
    signal,
    load: (s) => get('/api/changelog', { signal: s }),
    draw: (d) => {
      markVersionSeen(d.current);
      const releases = d.releases || [];
      if (!releases.length) return emptyState(t('pages.changelog.empty'), t('pages.changelog.empty_text'));
      return h('div', { class: 'cl-list' }, series(releases).map((g) => seriesSection(g, d.current)));
    },
  });
}
