// Upgrades ("Cluster" 36): the cluster's release (uploaded, or fetched from a published release and checked against
// its SHA256SUMS) and its rollout, a node at a time: workers first, then the managers, the leader last. Each step is
// shown as it goes, asked of the managers again every few seconds while the upgrade runs; it can be stopped (the node
// being upgraded finishes, no other starts). Every node's and manager's version is listed, those behind marked.

import { h, mount, bytes, when, ago } from '../dom.js';
import { get, post, put } from '../api.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { callout } from '../../finui/components/callout/callout.js';
import { facts } from '../../finui/components/facts/facts.js';
import { progressBar } from '../../finui/components/progress/progress.js';
import { toast } from '../../finui/components/toast/toast.js';
import { confirm } from '../confirm.js';
import { t, tOr } from '../i18n.js';

const EVERY_MS = 3000;
const RUN_TONES = { running: 'info', done: 'good', failed: 'critical', stopped: 'warning' };
const STEP_TONES = { waiting: 'info', upgrading: 'warning', done: 'good', skipped: 'warning', failed: 'critical' };
const sec = (ms) => Math.floor((ms || 0) / 1000);

/** Whether `a` is an older x.y.z than `b`; anything else is not known to be. */
export function olderThan(a, b) {
  const v = (s) => (/^\d+\.\d+\.\d+$/.test(s || '') ? s.split('.').map(Number) : null);
  const x = v(a), y = v(b);
  if (!x || !y) return false;
  for (let i = 0; i < 3; i++) if (x[i] !== y[i]) return x[i] < y[i];
  return false;
}

/** How many of the upgrade's steps are finished, and of how many. */
export function stepsDone(u) {
  const list = (u && u.steps) || [];
  return { done: list.filter((s) => ['done', 'skipped', 'failed'].includes(s.state)).length, all: list.length };
}

/** A version cell: the version, marked when it is behind `target`. */
function versionCell(v, target) {
  if (!v) return h('span', { class: 'sub' }, t('pages.upgrades.unknown'));
  return olderThan(v, target) ? h('span', { class: 'row nowrap' }, v, status({ tone: 'warning' }, t('pages.upgrades.behind'))) : v;
}

export function upgradesPage({ el, signal }) {
  const head = h('div'), body = h('div', { class: 'stack' });
  el.append(head, body);
  mount(head, pageHeader(t('pages.upgrades.title'), t('pages.upgrades.sub')));
  let timer = null;
  const reload = loadInto(body, {
    signal,
    load: (s) => get('/api/cluster/release', { signal: s }),
    draw: (v) => {
      clearTimeout(timer);
      const running = v.upgrade && v.upgrade.state === 'running';
      if (running) timer = setTimeout(() => { if (!signal.aborted) reload(); }, EVERY_MS);
      return h('div', { class: 'stack' }, releaseCard(v, reload), upgradeCard(v, reload), versionsCard(v));
    },
  });
  signal.addEventListener('abort', () => clearTimeout(timer));
}

function releaseCard(v, reload) {
  const r = v.release, running = v.upgrade && v.upgrade.state === 'running';
  const part = (p) => {
    const b = r && r[p];
    if (!b) return h('span', { class: 'sub' }, t('pages.upgrades.no_binary'));
    const here = v.here && v.here[p];
    return h('span', { class: 'row wrap' }, h('span', { class: 'mono', title: b.sha256 }, b.sha256.slice(0, 12)), h('span', { class: 'sub' }, bytes(b.size)),
      here ? null : status({ tone: 'info' }, t('pages.upgrades.copying')));
  };
  const rows = r ? [
    [t('pages.upgrades.version'), r.version],
    [t('pages.upgrades.arch'), r.arch],
    [t('pages.upgrades.worker_binary'), part('worker')],
    [t('pages.upgrades.manager_binary'), part('manager')],
    [t('pages.upgrades.source'), r.source === 'upload' ? t('pages.upgrades.uploaded') : h('span', { class: 'mono sub' }, r.source)],
    [t('pages.upgrades.given'), h('span', { title: when(sec(r.at)) }, ago(sec(r.at)))],
  ] : [];
  return card({
    title: t('pages.upgrades.release'), sub: t('pages.upgrades.release_sub'),
    actions: running ? null : h('div', { class: 'row' },
      button({ size: 'sm', onClick: () => fetchDialog(reload) }, t('pages.upgrades.fetch')),
      button({ size: 'sm', onClick: () => uploadDialog(v, reload) }, t('pages.upgrades.upload'))),
    body: r ? facts(rows) : emptyState(t('pages.upgrades.no_release'), t('pages.upgrades.no_release_text')),
  });
}

function upgradeCard(v, reload) {
  const u = v.upgrade, r = v.release;
  const running = u && u.state === 'running';
  const behind = r ? [...(v.nodes || []).map((n) => n.version), ...(v.managers || []).map((m) => m.version)].filter((x) => x !== r.version).length : 0;
  const actions = running
    ? button({ size: 'sm', variant: 'danger', onClick: () => stop(reload) }, t('pages.upgrades.stop'))
    : r && behind ? button({ size: 'sm', variant: 'primary', onClick: () => start(r, behind, reload) }, t('pages.upgrades.start', { version: r.version })) : null;
  if (!u) {
    return card({ title: t('pages.upgrades.upgrade'), sub: t('pages.upgrades.upgrade_sub'), actions,
      body: emptyState(t('pages.upgrades.none'), r && !behind ? t('pages.upgrades.all_on', { version: r.version }) : t('pages.upgrades.none_text')) });
  }
  const { done, all } = stepsDone(u);
  return card({
    title: t('pages.upgrades.upgrade_to', { version: u.version }), sub: t('pages.upgrades.upgrade_sub'), actions,
    body: h('div', { class: 'stack' },
      h('div', { class: 'row wrap' },
        status({ tone: RUN_TONES[u.state] || 'info', line: true }, tOr(`pages.upgrades.run.${u.state}`, u.state)),
        u.by ? h('span', { class: 'sub' }, t('pages.upgrades.by', { by: u.by })) : null,
        u.at ? h('span', { class: 'sub nowrap', title: when(sec(u.at)) }, ago(sec(u.at))) : null),
      u.state === 'failed' ? callout({ tone: 'critical', title: t('pages.upgrades.failed'), body: u.message }) : null,
      u.state === 'done' ? callout({ tone: 'good', title: t('pages.upgrades.done'), body: u.message }) : null,
      u.state === 'stopped' ? callout({ tone: 'warning', title: t('pages.upgrades.stopped'), body: u.message }) : null,
      progressBar({ label: t('pages.upgrades.progress'), value: all ? done / all : 0, detail: t('pages.upgrades.steps_done', { done, all }), tone: u.state === 'failed' ? 'critical' : 'accent' }),
      table({
        rows: u.steps || [], filter: false, dense: true, label: t('pages.upgrades.steps'),
        columns: [
          { label: '#', cell: (s) => h('span', { class: 'sub' }, String(u.steps.indexOf(s) + 1)), sort: (s) => u.steps.indexOf(s) },
          { label: t('pages.upgrades.what'), cell: (s) => tOr(`pages.upgrades.part.${s.part}`, s.part) },
          { label: t('pages.upgrades.which'), cell: (s) => h('span', { class: 'mono' }, s.name) },
          { label: t('pages.upgrades.how'), cell: (s) => status({ tone: STEP_TONES[s.state] || 'info' }, tOr(`pages.upgrades.step.${s.state}`, s.state)) },
          { label: t('pages.upgrades.said'), cell: (s) => h('span', { class: 'truncate sub', title: s.message || '' }, s.message || '') },
        ],
      })),
  });
}

function versionsCard(v) {
  const target = (v.release && v.release.version) || v.version;
  return card({
    title: t('pages.upgrades.versions'), sub: t('pages.upgrades.versions_sub', { version: v.version }),
    body: h('div', { class: 'stack' },
      table({
        rows: v.managers || [], filter: false, dense: true, label: t('pages.upgrades.managers'),
        columns: [
          { label: t('pages.upgrades.manager'), cell: (m) => h('span', { class: 'mono' }, m.addr), sort: (m) => m.addr },
          { label: t('pages.upgrades.version'), cell: (m) => versionCell(m.version, target) },
          { label: '', cell: (m) => (m.leader ? status({ tone: 'info' }, t('pages.upgrades.leader')) : '') },
        ],
      }),
      table({
        rows: v.nodes || [], dense: true, label: t('pages.upgrades.workers'),
        columns: [
          { label: t('pages.upgrades.worker'), cell: (n) => h('a', { href: `/cluster/nodes/${encodeURIComponent(n.id)}` }, n.name), sort: (n) => n.name },
          { label: t('pages.upgrades.version'), cell: (n) => versionCell(n.version, target), sort: (n) => n.version },
          { label: t('pages.upgrades.arch'), cell: (n) => n.arch || '' },
          { label: '', cell: (n) => (n.upgrade ? '' : h('span', { class: 'sub', title: t('pages.upgrades.by_hand_help') }, t('pages.upgrades.by_hand'))) },
        ],
      })),
  });
}

async function start(r, behind, reload) {
  if (!(await confirm({ title: t('pages.upgrades.start_title', { version: r.version }), body: t('pages.upgrades.start_body', { n: behind }), action: t('pages.upgrades.start', { version: r.version }), danger: false }))) return;
  try { const x = await post('/api/cluster/upgrade', {}); toast({ text: t('pages.upgrades.started', { n: x && x.steps ? x.steps : '' }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

async function stop(reload) {
  if (!(await confirm({ title: t('pages.upgrades.stop_title'), body: t('pages.upgrades.stop_body'), action: t('pages.upgrades.stop') }))) return;
  try { await post('/api/cluster/upgrade/stop', {}); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** The dialog for a binary: which (worker or manager), its version, the file. */
function uploadDialog(v, reload) {
  const body = h('div');
  const m = openModal({ title: t('pages.upgrades.upload'), body });
  const version = formField({ id: 'up-version', label: t('pages.upgrades.version'), placeholder: '1.1.0', help: t('pages.upgrades.version_help') });
  if (v.release) version.input.value = v.release.version;
  const file = formField({ id: 'up-file', label: t('pages.upgrades.file'), type: 'file', help: t('pages.upgrades.file_help', { arch: v.arch }) });
  const which = h('select', { class: 'fui-field__input', id: 'up-part' }, h('option', { value: 'worker' }, t('pages.upgrades.part.worker')), h('option', { value: 'manager' }, t('pages.upgrades.part.manager')));
  const said = h('p', { class: 'sub', 'aria-live': 'polite' });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.upgrades.upload'));
  const form = h('form', { class: 'stack', novalidate: true },
    h('div', { class: 'fui-field' }, h('label', { htmlFor: 'up-part', class: 'fui-field__label' }, t('pages.upgrades.which_binary')), which),
    version.el, file.el, said, h('div', { class: 'row row--end' }, go));
  mount(body, form);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const ver = version.input.value.trim(), f = file.input.files && file.input.files[0];
    if (!/^\d+\.\d+\.\d+$/.test(ver)) { version.setError(t('pages.upgrades.version_help')); return; }
    if (!f) { file.setError(t('pages.cluster.needed')); return; }
    go.disabled = true;
    said.textContent = t('pages.upgrades.uploading', { size: bytes(f.size) });
    try {
      await put(`/api/cluster/release/${which.value}?version=${encodeURIComponent(ver)}`, f);
      m.close();
      toast({ text: t('pages.upgrades.uploaded_toast', { which: t(`pages.upgrades.part.${which.value}`), version: ver }), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; said.textContent = x.message; }
  });
}

/** The dialog for a published release, by its tag. */
function fetchDialog(reload) {
  const body = h('div');
  const m = openModal({ title: t('pages.upgrades.fetch'), body });
  const tag = formField({ id: 'up-tag', label: t('pages.upgrades.tag'), placeholder: 'v1.1.0', help: t('pages.upgrades.tag_help') });
  const said = h('p', { class: 'sub', 'aria-live': 'polite' });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.upgrades.fetch'));
  const form = h('form', { class: 'stack', novalidate: true }, tag.el, said, h('div', { class: 'row row--end' }, go));
  mount(body, form);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const v = tag.input.value.trim();
    if (!/^v?\d+\.\d+\.\d+$/.test(v)) { tag.setError(t('pages.upgrades.tag_help')); return; }
    go.disabled = true;
    said.textContent = t('pages.upgrades.fetching');
    try {
      const r = await post('/api/cluster/release/fetch', { tag: v.startsWith('v') ? v : `v${v}` });
      m.close();
      toast({ text: t('pages.upgrades.fetched', { version: (r && r.release && r.release.version) || v }), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; said.textContent = x.message; }
  });
}
