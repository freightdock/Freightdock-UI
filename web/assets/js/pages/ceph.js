// Ceph made by the cluster ("Cluster" 34), on the Integrations page under the shared storage: an administrator picks
// the nodes and the free disks on each, names the storage and the filesystem, and the managers do the rest step by
// step (cephadm on each node, the first one bootstraps, the others join, monitors and managers placed, an OSD on each
// disk, the filesystem, its keys), the CephFS becoming storage like any other. Each step is shown as it goes, asked of
// the managers again every few seconds while it runs; a failed one can be run again, and a setup done or failed
// forgotten (what it made stays).

import { h, mount, bytes, when, ago } from '../dom.js';
import { get, post, del } from '../api.js';
import { loadInto, table } from '../view.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { callout } from '../../finui/components/callout/callout.js';
import { checkbox } from '../../finui/components/choice/choice.js';
import { progressBar } from '../../finui/components/progress/progress.js';
import { toast } from '../../finui/components/toast/toast.js';
import { confirm } from '../confirm.js';
import { t, tOr } from '../i18n.js';
import { nodeName } from './cluster.js';

const EVERY_MS = 3000;
const STATE_TONES = { running: 'info', failed: 'critical', done: 'good' };
const STEP_TONES = { pending: 'info', running: 'warning', done: 'good', failed: 'critical' };
const sec = (ms) => Math.floor((ms || 0) / 1000);

/** The Ceph card: the setup being made (or the last one) and the way to start one. `storageChanged` reloads the
 *  storage card, called once when a setup is done (its storage is new). */
export function cephCard({ signal, storageChanged }) {
  const slot = h('div');
  let timer = null, wasRunning = false;
  const reload = loadInto(slot, {
    signal,
    load: (s) => get('/api/cluster/ceph/setup', { signal: s }),
    draw: (c) => {
      const running = !!c && c.state === 'running';
      clearTimeout(timer);
      if (running) timer = setTimeout(() => { if (!signal.aborted) reload(); }, EVERY_MS);
      if (wasRunning && !running && c && c.state === 'done') storageChanged();
      wasRunning = running;
      return card({ title: t('pages.ceph.title'), sub: t('pages.ceph.sub'),
        actions: running ? null : button({ size: 'sm', onClick: () => setupDialog(reload) }, t('pages.ceph.set_up')),
        body: c ? setupView(c, reload) : emptyState(t('pages.ceph.none'), t('pages.ceph.none_text')) });
    },
  });
  signal.addEventListener('abort', () => clearTimeout(timer));
  return slot;
}

/** How far the setup is: its state, a bar of the steps done, and each step with its node and what it said. */
export function setupView(c, reload) {
  const list = c.steps || [];
  const done = list.filter((s) => s.state === 'done').length;
  const about = (s) => [s.about_name || '', s.disk || ''].filter(Boolean).join(' ');
  return h('div', { class: 'stack' },
    h('div', { class: 'row wrap' },
      status({ tone: STATE_TONES[c.state] || 'info', line: true }, tOr(`pages.ceph.state.${c.state}`, c.state)),
      h('span', { class: 'sub' }, t('pages.ceph.of', { storage: c.storage, fs: c.fs })),
      c.by ? h('span', { class: 'sub' }, t('pages.ceph.by', { by: c.by })) : null,
      c.updated_at ? h('span', { class: 'sub nowrap', title: when(sec(c.updated_at)) }, ago(sec(c.updated_at))) : null),
    c.state === 'failed' && c.message ? callout({ tone: 'critical', title: t('pages.ceph.failed'), body: c.message }) : null,
    c.state === 'done' ? callout({ tone: 'good', title: t('pages.ceph.done'), body: t('pages.ceph.done_text', { storage: c.storage }) }) : null,
    progressBar({ label: t('pages.ceph.progress'), value: list.length ? done / list.length : 0, detail: t('pages.ceph.steps_done', { done, all: list.length }), tone: c.state === 'failed' ? 'critical' : 'accent' }),
    table({
      rows: list, filter: false, dense: true, label: t('pages.ceph.steps'),
      columns: [
        { label: '#', cell: (s) => h('span', { class: 'sub' }, String(list.indexOf(s) + 1)), sort: (s) => list.indexOf(s) },
        { label: t('pages.ceph.step'), cell: (s) => tOr(`pages.ceph.kind.${s.kind}`, s.kind) },
        { label: t('pages.ceph.on'), cell: (s) => h('span', { class: 'mono' }, about(s)) },
        { label: t('pages.ceph.how'), cell: (s) => status({ tone: STEP_TONES[s.state] || 'info' }, tOr(`pages.ceph.step_state.${s.state}`, s.state)) },
        { label: t('pages.ceph.said'), cell: (s) => h('span', { class: 'truncate sub', title: s.message || '' }, lastLine(s.message)) },
      ],
    }),
    h('div', { class: 'row row--end' },
      c.state === 'failed' ? button({ size: 'sm', variant: 'primary', onClick: () => retry(reload) }, t('pages.ceph.retry')) : null,
      c.state !== 'running' ? button({ size: 'sm', onClick: () => clear(reload) }, t('pages.ceph.clear')) : null));
}

/** The last line of what a step said, which is what went wrong when it failed. */
export function lastLine(m) {
  return String(m || '').split('\n').map((l) => l.trim()).filter(Boolean).pop() || '';
}

async function retry(reload) {
  try { await post('/api/cluster/ceph/setup/retry', {}); toast({ text: t('pages.ceph.retried'), tone: 'info' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

async function clear(reload) {
  if (!(await confirm({ title: t('pages.ceph.clear_title'), body: t('pages.ceph.clear_body'), action: t('pages.ceph.clear'), danger: false }))) return;
  try { await del('/api/cluster/ceph/setup'); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** The nodes Ceph can be set up on: heard, at rest (not fenced) and with Docker. */
export function candidates(nodes) {
  return nodes.filter((n) => (n.liveness || {}).health === 'healthy' && ((n.fencing || {}).state || 'none') === 'none' && (n.report || {}).docker !== false);
}

/** What is sent: the hosts in the order chosen (the first bootstraps), each with its disks, nodes without one left out. */
export function setupBody({ storage, fs, image, order, chosen }) {
  return { storage: storage.trim(), fs: fs.trim(), image: image.trim(), hosts: order.filter((id) => (chosen.get(id) || []).length).map((id) => ({ node_id: id, disks: chosen.get(id) })) };
}

/** The dialog: every node that can take part with its disks (the ones not free say why), the names, and the start. */
async function setupDialog(reload) {
  const body = h('div', { class: 'stack' }, h('p', { class: 'sub' }, t('pages.ceph.looking')));
  const m = openModal({ title: t('pages.ceph.set_up'), body, wide: true });
  let nodes;
  try { nodes = candidates(await get('/api/cluster/nodes')); } catch (e) { mount(body, callout({ tone: 'critical', title: t('pages.ceph.no_nodes'), body: e.message })); return; }
  const disks = await Promise.all(nodes.map((n) => get(`/api/cluster/nodes/${encodeURIComponent(n.id)}/docker?action=disks`).then((d) => ({ list: d || [] }), (e) => ({ list: [], error: e.message }))));
  // Disks ticked, by node, in the order the nodes were first ticked: the first bootstraps.
  const chosen = new Map(), order = [];
  const tick = (id, path, on) => {
    const now = (chosen.get(id) || []).filter((p) => p !== path);
    if (on) now.push(path);
    chosen.set(id, now);
    if (now.length && !order.includes(id)) order.push(id);
    if (!now.length && order.includes(id)) order.splice(order.indexOf(id), 1);
    count.textContent = summary();
  };
  const summary = () => {
    const hosts = order.filter((id) => (chosen.get(id) || []).length);
    if (!hosts.length) return t('pages.ceph.pick');
    const first = nodes.find((n) => n.id === hosts[0]);
    return t(hosts.length >= 3 ? 'pages.ceph.picked' : 'pages.ceph.picked_few', { n: hosts.length, first: first ? nodeName(first) : '' });
  };
  const nodeBlock = (n, d) => h('fieldset', { class: 'fui-choice__group' }, h('legend', { class: 'fui-choice__legend' }, nodeName(n)),
    d.error ? h('p', { class: 'sub' }, d.error)
      : !d.list.length ? h('p', { class: 'sub' }, t('pages.ceph.no_disks'))
        // None free (a container sees its host's disks, none its own): one line saying why, not each disk greyed.
        : !d.list.some((x) => x.free) ? h('p', { class: 'sub' }, t('pages.ceph.none_free', { n: d.list.length, why: [...new Set(d.list.map((x) => x.why || t('pages.ceph.in_use')))].join('; ') }))
        : h('div', { class: 'fui-choice__options' }, d.list.map((x) => checkbox({
          label: `${x.path} · ${bytes(x.size || 0)}${x.model ? ` · ${x.model}` : ''}`,
          help: x.free ? null : (x.why || t('pages.ceph.in_use')), disabled: !x.free,
          onChange: (on) => tick(n.id, x.path, on) }))));
  const storage = formField({ id: 'ceph-storage', label: t('pages.ceph.storage'), placeholder: 'ceph', help: t('pages.ceph.storage_help') });
  const fs = formField({ id: 'ceph-fs', label: t('pages.ceph.fs'), placeholder: 'cephfs', help: t('pages.ceph.fs_help') });
  const image = formField({ id: 'ceph-image', label: t('pages.ceph.image'), placeholder: 'quay.io/ceph/ceph:v20', help: t('pages.ceph.image_help') });
  storage.input.value = 'ceph';
  fs.input.value = 'cephfs';
  const count = h('p', { class: 'sub', 'aria-live': 'polite' });
  const go = button({ variant: 'danger', type: 'submit' }, t('pages.ceph.start'));
  const form = h('form', { class: 'stack', novalidate: true },
    callout({ tone: 'warning', title: t('pages.ceph.erase'), body: t('pages.ceph.erase_text') }),
    nodes.length ? h('div', { class: 'stack' }, nodes.map((n, i) => nodeBlock(n, disks[i]))) : emptyState(t('pages.ceph.no_nodes'), t('pages.ceph.no_nodes_text')),
    count, storage.el, fs.el, image.el, h('div', { class: 'row row--end' }, go));
  count.textContent = summary();
  mount(body, form);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const b = setupBody({ storage: storage.input.value, fs: fs.input.value, image: image.input.value, order, chosen });
    if (!b.hosts.length) { count.textContent = t('pages.ceph.pick'); return; }
    if (!b.storage) { storage.setError(t('pages.cluster.needed')); return; }
    if (!b.fs) { fs.setError(t('pages.cluster.needed')); return; }
    const list = b.hosts.map((x) => `${nodeName(nodes.find((n) => n.id === x.node_id))}: ${x.disks.join(', ')}`).join('; ');
    if (!(await confirm({ title: t('pages.ceph.confirm_title'), body: t('pages.ceph.confirm_body', { disks: list }), action: t('pages.ceph.start') }))) return;
    go.disabled = true;
    try {
      const r = await post('/api/cluster/ceph/setup', b);
      m.close();
      toast({ text: t('pages.ceph.started', { n: r && r.steps ? r.steps : '' }), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; count.textContent = x.message; }
  });
}
