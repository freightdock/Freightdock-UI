// One node's Docker ("Cluster" 25): its containers, images, volumes and networks, asked of the node's worker through
// the managers when the page is drawn, for an Administrator. The cluster's own (a workload's container, a stack's
// network or volume) are shown, marked, and changed only through their stack: the worker refuses them here. A
// container's logs and inspect, a pull, a prune; every change goes to the activity log.

import { h, mount, bytes, ago, shortId } from '../dom.js';
import { get, post, qs } from '../api.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { facts } from '../../finui/components/facts/facts.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { callout } from '../../finui/components/callout/callout.js';
import { toast } from '../../finui/components/toast/toast.js';
import { confirm } from '../confirm.js';
import { t, tOr } from '../i18n.js';
import { nodeName } from './cluster.js';

const STATE_TONES = { running: 'good', restarting: 'warning', paused: 'warning', exited: 'critical', dead: 'critical', created: 'info' };
const DOCKER_OWN = ['bridge', 'host', 'none'];
/** Terminal colour and cursor codes, which a log read as text shows as noise. */
const ANSI = /\u001b\[[0-9;?]*[A-Za-z]/g;
/** An image as people read it: a bare digest shortened. */
const imageName = (i) => (/^sha256:[0-9a-f]{64}$/.test(i || '') ? shortId(i) : i || '');

export function clusterDockerPage({ el, params, signal }) {
  const id = params.id;
  const base = `/api/cluster/nodes/${encodeURIComponent(id)}`;
  const read = (action, q = {}, s = signal) => get(`${base}/docker${qs({ action, ...q })}`, { signal: s });
  const change = (action, target = '', p = {}) => post(`${base}/docker`, { action, target, params: p });
  const head = h('div'), info = h('div'), body = h('div');
  el.append(head, info, body);
  mount(head, pageHeader(t('pages.cluster_docker.title'), t('pages.cluster_docker.sub'), button({ size: 'sm', href: `/cluster/nodes/${encodeURIComponent(id)}` }, t('pages.cluster_docker.back'))));
  get(base, { signal }).then((n) => mount(head, pageHeader(t('pages.cluster_docker.title_of', { name: nodeName(n) }), t('pages.cluster_docker.sub'),
    button({ size: 'sm', href: `/cluster/nodes/${encodeURIComponent(id)}` }, t('pages.cluster_docker.back'))))).catch(() => {});
  loadInto(info, { signal, load: (s) => read('info', {}, s), draw: (i) => card({ title: t('pages.cluster_docker.engine'), body: engine(i) }) });

  const ctx = { read, change, signal };
  mount(body, card({ body: tabs({
    label: t('pages.cluster_docker.title'),
    tabs: [
      { key: 'containers', label: t('pages.cluster_docker.containers'), panel: () => panel(ctx, 'containers', containerTable) },
      { key: 'images', label: t('pages.cluster_docker.images'), panel: () => panel(ctx, 'images', imageTable) },
      { key: 'volumes', label: t('pages.cluster_docker.volumes'), panel: () => panel(ctx, 'volumes', volumeTable) },
      { key: 'networks', label: t('pages.cluster_docker.networks'), panel: () => panel(ctx, 'networks', networkTable) },
    ],
  }) }));
}

function engine(i) {
  return facts([
    [t('pages.cluster_docker.version'), `${i.version || ''} (API ${i.api_version || ''})`],
    [t('pages.cluster_docker.os'), i.os || ''],
    [t('pages.cluster_docker.kernel'), i.kernel || '', { mono: true }],
    [t('pages.cluster_docker.containers'), t('pages.cluster_docker.running_of', { running: String(i.running ?? 0), all: String(i.containers ?? 0) })],
    [t('pages.cluster_docker.images'), String(i.images ?? 0)],
  ]);
}

/** One tab: its list asked when shown, drawn by `draw(rows, ctx)`, asked again after a change. */
function panel(ctx, action, draw) {
  const slot = h('div');
  const c = { ...ctx };
  c.reload = loadInto(slot, { signal: ctx.signal, load: (s) => ctx.read(action, {}, s), draw: (rows) => draw(rows || [], c) });
  return slot;
}

/** A change: asked to confirm when it removes, then done, said, and the list asked again. */
async function act(c, { action, target = '', params = {}, ask = null, done }) {
  if (ask && !(await confirm(ask))) return;
  try {
    await c.change(action, target, params);
    toast({ text: done, tone: 'good' });
    c.reload();
  } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

const ours = (x) => (x.cluster ? status({ tone: 'info' }, x.stack ? t('pages.cluster_docker.of_stack', { stack: x.stack }) : t('pages.cluster_docker.cluster_s')) : null);

function containerTable(rows, c) {
  const name = (x) => (x.names || [])[0] || shortId(x.id);
  return table({
    rows, dense: true, label: t('pages.cluster_docker.containers'), empty: [t('pages.cluster_docker.no_containers'), ''],
    columns: [
      { label: t('pages.cluster_docker.name'), cell: (x) => h('div', { class: 'row' }, h('span', { class: 'mono' }, name(x)), ours(x)), sort: name },
      { label: t('pages.cluster_docker.image'), cell: (x) => h('span', { class: 'mono sub', title: x.image || '' }, imageName(x.image)), sort: (x) => x.image },
      { label: t('pages.cluster_docker.state'), cell: (x) => h('div', { class: 'stack' }, status({ tone: STATE_TONES[x.state] || 'info' }, tOr(`pages.cluster_node.state.${x.state}`, x.state || '')), h('span', { class: 'sub' }, x.status || '')), sort: (x) => x.state },
      { label: t('pages.cluster_docker.ports'), cell: (x) => h('span', { class: 'mono sub' }, ports(x.ports)) },
      { label: '', nosort: true, cell: (x) => h('div', { class: 'row row--end nowrap fd-docker-actions' },
        button({ size: 'sm', onClick: () => logs(c, x.id, name(x)) }, t('pages.cluster_docker.logs')),
        button({ size: 'sm', onClick: () => inspect(c, x.id, name(x)) }, t('pages.cluster_docker.inspect')),
        x.cluster ? null : x.state === 'running'
          ? button({ size: 'sm', onClick: () => act(c, { action: 'container.stop', target: x.id, done: t('pages.cluster_docker.stopped', { name: name(x) }) }) }, t('pages.cluster_docker.stop'))
          : button({ size: 'sm', onClick: () => act(c, { action: 'container.start', target: x.id, done: t('pages.cluster_docker.started', { name: name(x) }) }) }, t('pages.cluster_docker.start')),
        x.cluster ? null : button({ size: 'sm', onClick: () => act(c, { action: 'container.restart', target: x.id, done: t('pages.cluster_docker.restarted', { name: name(x) }) }) }, t('pages.cluster_docker.restart')),
        x.cluster ? null : button({ size: 'sm', variant: 'danger', onClick: () => act(c, { action: 'container.remove', target: x.id, params: { force: 'true' },
          ask: { title: t('pages.cluster_docker.remove_container', { name: name(x) }), body: t('pages.cluster_docker.remove_container_body') }, done: t('pages.cluster_docker.removed', { name: name(x) }) }) }, t('common.remove'))) },
    ],
  });
}

function ports(list) {
  return (list || []).filter((p) => p.public).map((p) => `${p.public}:${p.private}/${p.type || 'tcp'}`).filter((v, i, a) => a.indexOf(v) === i).join(', ');
}

/** A container's last lines of log, as many as chosen, asked again on demand. */
function logs(c, target, name) {
  let tail = '500';
  const out = h('pre', { class: 'pre mono fd-docker-log' });
  const note = h('p', { class: 'sub' });
  const load = async () => {
    out.textContent = t('pages.cluster_docker.loading');
    try {
      const r = await c.read('logs', { target, tail });
      out.textContent = (r.lines || []).join('\n').replace(ANSI, '') || t('pages.cluster_docker.no_lines');
      note.textContent = r.cut ? t('pages.cluster_docker.cut') : '';
      out.scrollTop = out.scrollHeight;
    } catch (e) { out.textContent = e.message; }
  };
  const seg = segmented({ label: t('pages.cluster_docker.lines'), value: tail, size: 'sm', options: ['100', '500', '5000'].map((v) => ({ value: v, label: v })), onChange: (v) => { tail = v; load(); } });
  openModal({ title: t('pages.cluster_docker.logs_of', { name }), wide: true, body: h('div', { class: 'stack' },
    h('div', { class: 'row' }, seg, button({ size: 'sm', onClick: load }, t('pages.cluster_docker.refresh'))), out, note) });
  load();
}

function inspect(c, target, name) {
  const out = h('pre', { class: 'pre mono fd-docker-log' }, t('pages.cluster_docker.loading'));
  openModal({ title: t('pages.cluster_docker.inspect_of', { name }), wide: true, body: out });
  c.read('container', { target }).then((v) => { out.textContent = JSON.stringify(v, null, 2); }).catch((e) => { out.textContent = e.message; });
}

function imageTable(rows, c) {
  const tag = (x) => ((x.tags || []).filter((n) => n !== '<none>:<none>')[0]) || shortId(x.id);
  const tools = h('div', { class: 'row row--end' },
    button({ size: 'sm', onClick: () => pull(c) }, t('pages.cluster_docker.pull')),
    button({ size: 'sm', onClick: () => act(c, { action: 'images.prune', ask: { title: t('pages.cluster_docker.prune_title'), body: t('pages.cluster_docker.prune_body'), action: t('pages.cluster_docker.prune') }, done: t('pages.cluster_docker.pruned') }) }, t('pages.cluster_docker.prune')));
  return h('div', { class: 'stack' }, tools, table({
    rows, dense: true, label: t('pages.cluster_docker.images'), empty: [t('pages.cluster_docker.no_images'), ''],
    columns: [
      { label: t('pages.cluster_docker.tags'), cell: (x) => h('div', { class: 'stack' }, ((x.tags || []).length ? x.tags : [shortId(x.id)]).map((n) => h('span', { class: 'mono' }, n))), sort: tag },
      { label: t('pages.cluster_docker.id'), cell: (x) => h('span', { class: 'mono sub', title: x.id }, shortId(x.id)) },
      { label: t('pages.cluster_docker.size'), cell: (x) => bytes(x.size), sort: (x) => x.size },
      { label: t('pages.cluster_docker.created'), cell: (x) => ago(x.created), sort: (x) => x.created },
      { label: '', nosort: true, cell: (x) => h('div', { class: 'row row--end' }, button({ size: 'sm', variant: 'danger', onClick: () => act(c, { action: 'image.remove', target: x.id,
        ask: { title: t('pages.cluster_docker.remove_image', { name: tag(x) }), body: t('pages.cluster_docker.remove_image_body') }, done: t('pages.cluster_docker.removed', { name: tag(x) }) }) }, t('common.remove'))) },
    ],
  }));
}

/** Pulls an image onto this node only (a stack's images are pulled where its workloads run). */
function pull(c) {
  const f = formField({ id: 'dk-pull', label: t('pages.cluster_docker.image'), placeholder: 'nginx:1.27', autocomplete: 'off', help: t('pages.cluster_docker.pull_help') });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.cluster_docker.pull'));
  const form = h('form', { class: 'stack', novalidate: true }, f.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.cluster_docker.pull_title'), body: form, initialFocus: f.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const image = f.input.value.trim();
    if (!image) { f.setError(t('pages.cluster_docker.pull_help')); return; }
    go.disabled = true;
    go.textContent = t('pages.cluster_docker.pulling');
    try {
      await c.change('image.pull', '', { image });
      m.close();
      toast({ text: t('pages.cluster_docker.pulled', { name: image }), tone: 'good' });
      c.reload();
    } catch (x) { go.disabled = false; go.textContent = t('pages.cluster_docker.pull'); f.setError(x.message); }
  });
}

function volumeTable(rows, c) {
  const short = (n) => (/^[0-9a-f]{64}$/.test(n || '') ? shortId(n) : n);
  return h('div', { class: 'stack' }, callout({ tone: 'info', body: t('pages.cluster_docker.volumes_note') }), table({
    rows, dense: true, label: t('pages.cluster_docker.volumes'), empty: [t('pages.cluster_docker.no_volumes'), ''],
    columns: [
      { label: t('pages.cluster_docker.name'), cell: (x) => h('div', { class: 'row' }, h('span', { class: 'mono', title: x.name }, short(x.name)), ours(x)), sort: (x) => x.name },
      { label: t('pages.cluster_docker.driver'), cell: (x) => x.driver || '', sort: (x) => x.driver },
      { label: t('pages.cluster_docker.created'), cell: (x) => (x.created ? ago(Date.parse(x.created) / 1000) : ''), sort: (x) => x.created },
      { label: '', nosort: true, cell: (x) => (x.cluster ? '' : h('div', { class: 'row row--end' }, button({ size: 'sm', variant: 'danger', onClick: () => act(c, { action: 'volume.remove', target: x.name,
        ask: { title: t('pages.cluster_docker.remove_volume', { name: short(x.name) }), body: t('pages.cluster_docker.remove_volume_body') }, done: t('pages.cluster_docker.removed', { name: short(x.name) }) }) }, t('common.remove')))) },
    ],
  }));
}

function networkTable(rows, c) {
  return table({
    rows, dense: true, label: t('pages.cluster_docker.networks'), empty: [t('pages.cluster_docker.no_networks'), ''],
    columns: [
      { label: t('pages.cluster_docker.name'), cell: (x) => h('div', { class: 'row' }, h('span', { class: 'mono' }, x.name || ''), ours(x)), sort: (x) => x.name },
      { label: t('pages.cluster_docker.driver'), cell: (x) => x.driver || '', sort: (x) => x.driver },
      { label: t('pages.cluster_docker.scope'), cell: (x) => [x.scope || '', x.internal ? t('pages.cluster_docker.internal') : ''].filter(Boolean).join(', ') },
      { label: '', nosort: true, cell: (x) => (x.cluster || DOCKER_OWN.includes(x.name) ? '' : h('div', { class: 'row row--end' }, button({ size: 'sm', variant: 'danger', onClick: () => act(c, { action: 'network.remove', target: x.id,
        ask: { title: t('pages.cluster_docker.remove_network', { name: x.name }), body: t('pages.cluster_docker.remove_network_body') }, done: t('pages.cluster_docker.removed', { name: x.name }) }) }, t('common.remove')))) },
    ],
  });
}
