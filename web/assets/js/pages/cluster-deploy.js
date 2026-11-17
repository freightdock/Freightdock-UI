// The stack deployer ("Cluster" 19): a new stack at /cluster/stacks/new, a stack changed at /cluster/stacks/<name>/edit.
// Three parts, checked by the managers as they are written (POST /api/cluster/stack-check, nothing deployed): the
// Compose file, with the services it holds and what the managers set aside; where it runs, any healthy worker or the
// workers chosen, with where its new workloads would go; and how it is reached from outside, its exposures on the
// pool. A Compose port opens nothing by itself: it is offered here as an exposure on a free pool address, which the
// person deploying keeps, moves or makes internal only. Nothing else leaves the cluster ("Cluster" 17). Beside the
// file, the stack's options ("Cluster" 22): a description, an env file for its variables, a node label selector, how
// it survives its node (as the file says, stateless, pinned, or on shared storage) and its containers' own names.
// Under the file, its storage ("Cluster" 39): a volume added on the storage chosen, named for the stack and never one
// the file has, and the file's relative bind mounts put on that storage in one go. A volume put on a storage of the
// cluster makes the stack's survival "Shared storage" on it, and the workers that cannot keep data there are hidden
// from the ones to choose, with why ("Cluster" 40).

import { h, mount } from '../dom.js';
import { get, post } from '../api.js';
import { isAdmin } from '../state.js';
import { navigate } from '../router.js';
import { codeEditor } from '../editor.js';
import { unfit } from '../storage-fit.js';
import { volumeName, serviceNames, declaredVolumes, addVolume, relativeBinds, convertBinds } from '../compose-volumes.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { formField } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { checkbox } from '../../finui/components/choice/choice.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t, tOr } from '../i18n.js';
import { nodeName, labelText } from './cluster.js';

// A Compose file to start from: Compose's own words. Its port is offered on the pool below, never opened by itself.
const EXAMPLE = `services:
  web:
    image: nginx:alpine
    ports: ["8080:80"]
    deploy:
      replicas: 2
`;
const NAME = /^[a-z0-9][a-z0-9-]{0,39}$/;
const CHECK_AFTER_MS = 600;
const HEALTH_TONES = { healthy: 'good', suspect: 'warning', unreachable: 'critical' };

export function clusterDeployPage({ el, params, signal }) {
  const existing = params && params.name ? params.name : null;
  const head = h('div'), body = h('div', { class: 'stack' });
  el.append(head, body);
  const back = button({ size: 'sm', href: '/cluster/stacks' }, t('pages.cluster_deploy.back'));
  mount(head, pageHeader(existing ? t('pages.cluster_stacks.edit_title', { name: existing }) : t('pages.cluster_deploy.title'), t('pages.cluster_deploy.sub'), back));
  if (!isAdmin()) { mount(body, emptyState(t('pages.cluster_deploy.admin_only'), '')); return; }
  (async () => {
    let nodes, pool, storage, stack = null;
    try {
      [nodes, pool, storage, stack] = await Promise.all([
        get('/api/cluster/nodes', { signal }), get('/api/cluster/floating', { signal }).catch(() => []), get('/api/cluster/storage', { signal }).catch(() => []),
        existing ? get(`/api/cluster/stacks/${encodeURIComponent(existing)}`, { signal }) : null]);
    } catch (e) { if (!signal.aborted) mount(body, emptyState(t('pages.cluster.no_answer'), e.message)); return; }
    if (!signal.aborted) mount(body, deployer({ existing, stack, nodes: nodes || [], pool: pool || [], storage: storage || [], signal }));
  })();
}

function deployer({ existing, stack, nodes, pool, storage, signal }) {
  // The workers chosen (ids); none while it runs on any.
  let mode = stack && (stack.nodes || []).length ? 'chosen' : 'any';
  const chosen = new Set(stack ? stack.nodes || [] : []);
  let last = null;
  const had = (stack && stack.options) || {};
  // Each service's own container name, as typed here; the file's container_name when left empty.
  const names = new Map(Object.entries(had.names || {}));
  let survive = had.mode ? had.mode.kind : 'file';

  // 1. The Compose file.
  const name = formField({ id: 'cs-name', label: t('pages.cluster_stacks.name'), autocomplete: 'off', help: t('pages.cluster_stacks.name_help') });
  name.input.value = existing || '';
  if (existing) name.input.readOnly = true;
  const file = codeEditor({ id: 'cs-compose', label: t('pages.cluster_stacks.compose'), value: stack ? stack.compose || '' : EXAMPLE, rows: 20, language: 'yaml', help: t('pages.cluster_deploy.compose_help'), onChange: () => later() });
  name.input.addEventListener('input', () => later());
  const about = h('textarea', { class: 'fui-field__input fui-field__input--area', id: 'cs-about', rows: 2, maxLength: 500 }, had.description || '');
  const aboutField = h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: 'cs-about' }, t('pages.cluster_deploy.description')), about,
    h('p', { class: 'fui-field__help' }, t('pages.cluster_deploy.description_help')));
  const env = h('textarea', { class: 'fui-field__input fui-field__input--area mono', id: 'cs-env', rows: 4, spellcheck: false, placeholder: 'TAG=1.2\nPASSWORD=...' }, had.env || '');
  env.addEventListener('input', () => later());
  const envField = h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: 'cs-env' }, t('pages.cluster_deploy.env')), env,
    h('p', { class: 'fui-field__help' }, t('pages.cluster_deploy.env_help')));
  const found = h('div', { class: 'stack', 'aria-live': 'polite' });
  const volumes = volumeTool({ file, storage, stackName: () => name.input.value.trim(), changed: () => later(0), picked: (st) => {
    survive = 'shared';
    survivePick.setValue('shared');
    onStorage.value = st;
    drawSurvival();
    drawWorkers();
  } });

  // 2. Where it runs.
  const workers = h('div', { class: 'fd-deploy__nodes' });
  const placed = h('div', { 'aria-live': 'polite' });
  // The storages the stack keeps data on: its file's volumes, as the managers read them, and its survival's.
  const used = () => [...new Set([...Object.values((last && last.volumes) || {}).map((v) => v.storage), ...(survive === 'shared' && onStorage.value ? [onStorage.value] : [])])];
  const drawWorkers = () => {
    const bad = unfit(nodes, storage, used());
    for (const id of bad.keys()) chosen.delete(id);
    const fit = nodes.filter((n) => !bad.has(n.id));
    const why = bad.size ? status({ tone: 'info', line: true }, t('pages.cluster_deploy.unfit', { count: bad.size, list: nodes.filter((n) => bad.has(n.id)).map((n) => {
      const r = bad.get(n.id);
      return `${nodeName(n)} (${r.text || t('pages.cluster_deploy.unfit_driver', { storage: r.storage, driver: r.driver })})`;
    }).join(', ') })) : null;
    mount(workers, mode === 'any' ? h('div', { class: 'stack' }, h('p', { class: 'sub' }, t('pages.cluster_deploy.any_help')), why) : h('div', { class: 'stack' },
    h('p', { class: 'sub' }, t('pages.cluster_deploy.chosen_help')),
    fit.length ? h('div', { class: 'fd-deploy__node-list' }, fit.map((n) => {
      const health = (n.liveness && n.liveness.health) || 'unknown';
      return h('div', { class: 'row fd-deploy__node' },
        checkbox({ label: nodeName(n), checked: chosen.has(n.id), onChange: (on) => { if (on) chosen.add(n.id); else chosen.delete(n.id); later(0); } }),
        status({ tone: HEALTH_TONES[health] || 'info', line: true }, tOr(`pages.cluster.health_word.${health}`, health)));
    })) : h('p', { class: 'sub' }, t('pages.cluster.no_nodes')), why));
  };
  const where = segmented({ label: t('pages.cluster_deploy.where'), value: mode, size: 'sm',
    options: [{ value: 'any', label: t('pages.cluster_deploy.any') }, { value: 'chosen', label: t('pages.cluster_deploy.chosen') }],
    onChange: (v) => { mode = v; drawWorkers(); later(0); } });
  const selector = formField({ id: 'cs-selector', label: t('pages.cluster_deploy.selector'), autocomplete: 'off', placeholder: t('pages.cluster_deploy.selector_placeholder'), help: t('pages.cluster_deploy.selector_help') });
  selector.input.value = (had.selector || []).map(labelText).join(', ');
  selector.input.addEventListener('input', () => later());
  const labelsSeen = [...new Set(nodes.flatMap((n) => Object.entries(Object.assign({}, (n.report && n.report.labels) || {}, n.labels || {})).map(([k, v]) => `${k}=${v}`)))].sort();
  const seen = labelsSeen.length ? h('p', { class: 'sub' }, t('pages.cluster_deploy.labels_seen', { labels: labelsSeen.join(', ') })) : null;

  // How it survives its node.
  const storageNames = storage.map((x) => x.name);
  const onStorage = h('select', { class: 'fui-field__input', id: 'cs-storage' }, storageNames.map((n) => h('option', { value: n, selected: had.mode && had.mode.storage === n }, n)));
  onStorage.addEventListener('change', () => { drawWorkers(); later(0); });
  drawWorkers();
  const survival = h('div', { class: 'stack' });
  const drawSurvival = () => mount(survival, h('p', { class: 'sub' }, t(`pages.cluster_deploy.mode_help.${survive}`)),
    survive === 'shared' ? (storageNames.length ? h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: 'cs-storage' }, t('pages.cluster_deploy.mode_storage')), onStorage)
      : status({ tone: 'warning', line: true }, t('pages.cluster_deploy.mode_no_storage'))) : null);
  const survivePick = segmented({ label: t('pages.cluster_deploy.mode'), value: survive, size: 'sm',
    options: ['file', 'stateless', 'pinned', 'shared'].map((v) => ({ value: v, label: t(`pages.cluster_deploy.mode_word.${v}`) })),
    onChange: (v) => { survive = v; drawSurvival(); drawWorkers(); later(0); } });
  drawSurvival();

  // 3. How it is reached from outside.
  const reach = exposureEditor({ pool, stack: existing, initial: stack ? stack.exposures || [] : [] });

  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  // A new stack can be made without being started ("Cluster" 31): to look it over, or while its old copy still runs.
  let cold = false;
  const coldBox = existing ? null : checkbox({ label: t('pages.cluster_deploy.stopped'), checked: false, onChange: (on) => { cold = on; } });
  const go = button({ variant: 'primary', type: 'submit' }, existing ? t('pages.cluster_deploy.redeploy') : t('pages.cluster_stacks.deploy'));
  const form = h('form', { class: 'stack', novalidate: true },
    card({ title: t('pages.cluster_deploy.file'), body: h('div', { class: 'stack' }, name.el, aboutField, file.el, volumes.el, envField, found) }),
    card({ title: t('pages.cluster_deploy.where'), sub: t('pages.cluster_deploy.where_sub'), actions: where, body: h('div', { class: 'stack' }, workers, selector.el, seen, placed) }),
    card({ title: t('pages.cluster_deploy.mode'), sub: t('pages.cluster_deploy.mode_sub'), actions: survivePick, body: survival }),
    card({ title: t('pages.cluster_stacks.reach'), sub: t('pages.cluster_deploy.reach_sub'), body: reach.el }),
    err,
    coldBox ? h('p', { class: 'sub' }, t('pages.cluster_deploy.stopped_help')) : null,
    h('div', { class: 'row row--end' }, coldBox, button({ href: '/cluster/stacks' }, t('common.cancel')), go));

  const options = () => ({
    description: about.value.trim(), env: env.value,
    selector: selector.input.value.split(',').map((x) => x.trim()).filter(Boolean),
    mode: survive === 'file' ? null : survive === 'shared' ? { kind: 'shared', storage: onStorage.value } : { kind: survive },
    names: Object.fromEntries([...names].map(([k, v]) => [k, v.trim()])),
  });
  const body = () => ({ name: name.input.value.trim(), compose: file.value, nodes: mode === 'any' ? [] : [...chosen], exposures: reach.read(), ...options() });

  // The managers' check, a moment after the last edit; a newer one aborts the one before.
  let timer = null, inflight = null;
  function later(ms = CHECK_AFTER_MS) { clearTimeout(timer); timer = setTimeout(check, ms); }
  async function check() {
    if (inflight) inflight.abort();
    const ctl = new AbortController();
    inflight = ctl;
    signal.addEventListener('abort', () => ctl.abort(), { once: true });
    const b = body();
    if (!NAME.test(b.name)) b.name = existing || 'new-stack';
    if (mode === 'chosen' && !b.nodes.length) b.nodes = [];
    mount(found, h('p', { class: 'sub' }, t('pages.cluster_deploy.checking')));
    try {
      const { exposures, ...asked } = b;
      last = await post('/api/cluster/stack-check', asked, { signal: ctl.signal });
      file.setError('');
      volumes.refresh();
      drawWorkers();
      mount(found, servicesFound(last, b.name, names, () => later(0)));
      mount(placed, placement(last, nodes, mode === 'chosen' && !chosen.size));
      reach.offer(last.services || {}, !existing);
    } catch (e) {
      if (ctl.signal.aborted) return;
      last = null;
      file.setError(e.message);
      volumes.refresh();
      mount(found);
      mount(placed);
    }
  }
  check();

  form.addEventListener('submit', async (ev) => {
    ev.preventDefault();
    err.textContent = '';
    const b = body();
    if (!NAME.test(b.name)) { name.setError(t('pages.cluster_stacks.name_help')); name.input.focus(); return; }
    name.setError('');
    if (mode === 'chosen' && !b.nodes.length) { err.textContent = t('pages.cluster_deploy.choose_one'); return; }
    if (b.exposures === null) { err.textContent = t('pages.cluster_stacks.exposure_incomplete'); return; }
    go.disabled = true;
    try {
      const r = await post('/api/cluster/stacks', cold ? { ...b, stopped: true } : b);
      toast({ text: t('pages.cluster_stacks.deployed', { name: b.name }), tone: 'good' });
      for (const w of r.warnings || []) toast({ text: w, tone: 'warning' });
      navigate('/cluster/stacks');
    } catch (x) { go.disabled = false; err.textContent = x.message; }
  });
  return form;
}

/** Storage for the stack, under its file: where the data is kept (a storage of the cluster, or the node's own disk),
 *  the service and the path in it, and Add volume, as often as one likes; each new volume's name is the stack's with
 *  a random part, never one the file has. When the file mounts relative paths (./data, which mean nothing on a worker),
 *  a button puts them all on the storage chosen. */
function volumeTool({ file, storage, stackName, changed, picked }) {
  const LOCAL = '';
  const where = h('select', { class: 'fui-field__input', id: 'cs-vol-where' },
    storage.map((x) => h('option', { value: x.name }, x.driver ? `${x.name} (${x.driver})` : x.name)),
    h('option', { value: LOCAL }, t('pages.cluster_deploy.vol_local')));
  const svc = h('select', { class: 'fui-field__input', id: 'cs-vol-service' });
  const target = h('input', { class: 'fui-field__input mono', id: 'cs-vol-target', autocomplete: 'off', spellcheck: false, placeholder: '/data' });
  const said = h('p', { class: 'sub', 'aria-live': 'polite' });
  const binds = h('div');
  const field = (id, label, input) => h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: id }, label), input);
  const storageWord = () => (where.value === LOCAL ? t('pages.cluster_deploy.vol_local') : where.value);

  function refresh() {
    let list = [];
    try { list = serviceNames(file.value); } catch { list = []; }
    const keep = svc.value;
    mount(svc, h('option', { value: '' }, t('pages.cluster_deploy.vol_no_service')), list.map((n) => h('option', { value: n }, n)));
    // The one chosen stays; with none chosen and one service, that one.
    svc.value = list.includes(keep) ? keep : list.length === 1 ? list[0] : '';
    let found = [];
    try { found = relativeBinds(file.value); } catch { found = []; }
    mount(binds, found.length ? h('div', { class: 'row wrap' },
      status({ tone: 'warning', line: true }, t('pages.cluster_deploy.vol_relative', { count: found.length, paths: [...new Set(found.map((b) => b.source))].join(', ') })),
      button({ size: 'sm', onClick: convert }, t('pages.cluster_deploy.vol_convert', { where: storageWord() }))) : null);
  }

  function add() {
    said.textContent = '';
    const to = target.value.trim();
    if (svc.value && !to.startsWith('/')) { said.textContent = t('pages.cluster_deploy.vol_target_needed'); target.focus(); return; }
    const vol = volumeName(stackName(), declaredVolumes(file.value));
    try {
      file.value = addVolume(file.value, { name: vol, storage: where.value || null, service: svc.value || null, target: to });
    } catch (e) { said.textContent = e.key ? t(`pages.cluster_deploy.vol_err.${e.key}`, e.vars) : e.message; return; }
    said.textContent = svc.value ? t('pages.cluster_deploy.vol_added_mounted', { name: vol, where: storageWord(), service: svc.value, target: to })
      : t('pages.cluster_deploy.vol_added', { name: vol, where: storageWord() });
    if (where.value !== LOCAL) said.textContent += ` ${t('pages.cluster_deploy.vol_picked', { where: where.value })}`;
    target.value = '';
    if (where.value !== LOCAL) picked(where.value);
    refresh();
    changed();
  }

  function convert() {
    said.textContent = '';
    let r;
    try { r = convertBinds(file.value, { storage: where.value || null, stack: stackName() }); } catch (e) { said.textContent = e.key ? t(`pages.cluster_deploy.vol_err.${e.key}`, e.vars) : e.message; return; }
    if (!r.changed.length) return;
    file.value = r.text;
    if (where.value !== LOCAL) picked(where.value);
    said.textContent = t('pages.cluster_deploy.vol_converted', { list: r.changed.map((c) => `${c.source} \u2192 ${c.name}`).join(', '), where: storageWord() });
    if (where.value !== LOCAL) said.textContent += ` ${t('pages.cluster_deploy.vol_picked', { where: where.value })}`;
    refresh();
    changed();
  }

  const go = button({ size: 'sm', onClick: add }, t('pages.cluster_deploy.vol_add'));
  // Enter here adds the volume; it would deploy the stack otherwise.
  target.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); add(); } });
  refresh();
  const el = h('div', { class: 'stack fd-deploy__volumes' },
    h('div', { class: 'fui-field__label' }, t('pages.cluster_deploy.vol_title')),
    h('p', { class: 'sub' }, t('pages.cluster_deploy.vol_help')),
    h('div', { class: 'row wrap row--bottom' }, field('cs-vol-where', t('pages.cluster_deploy.vol_where'), where), field('cs-vol-service', t('pages.cluster_deploy.vol_service'), svc), field('cs-vol-target', t('pages.cluster_deploy.vol_target'), target), go),
    binds, said);
  return { el, refresh };
}

/** The services the managers found in the file, each with its containers' own name to type (`names`, by service),
 *  and the warnings for what they set aside. */
function servicesFound(c, stackName, names, changed) {
  const list = Object.entries(c.services || {});
  const nameOf = ([k, v]) => {
    const input = h('input', { class: 'fui-field__input mono', id: `cs-n-${k}`, autocomplete: 'off', spellcheck: false, value: names.get(k) || '',
      placeholder: v.name && !names.get(k) ? v.name : `${stackName}-${k}-1`, 'aria-label': t('pages.cluster_deploy.container_name', { service: k }) });
    input.addEventListener('input', () => names.set(k, input.value));
    input.addEventListener('change', () => changed());
    return h('div', { class: 'row fd-deploy__name' }, h('label', { class: 'mono', htmlFor: `cs-n-${k}` }, k), input);
  };
  return h('div', { class: 'stack' },
    h('div', { class: 'row wrap' }, h('span', { class: 'sub' }, t('pages.cluster_deploy.found', { n: list.length })),
      list.map(([k, v]) => h('span', { class: 'fd-deploy__svc' }, h('span', { class: 'mono' }, k), h('span', { class: 'sub' }, ` ${v.image}, ${v.replicas}x`)))),
    list.length ? h('div', { class: 'stack' }, h('p', { class: 'sub' }, t('pages.cluster_deploy.names_help')), list.map(nameOf)) : null,
    Object.keys(c.volumes || {}).length ? h('div', { class: 'row wrap' }, h('span', { class: 'sub' }, t('pages.cluster_deploy.volumes')),
      Object.entries(c.volumes).map(([k, v]) => h('span', { class: 'fd-deploy__svc' }, h('span', { class: 'mono' }, k),
        h('span', { class: 'sub' }, ` ${t(v.access === 'many' ? 'pages.cluster_deploy.volume_many' : 'pages.cluster_deploy.volume_one', { storage: v.storage })}`)))) : null,
    (c.warnings || []).length ? h('ul', { class: 'fd-deploy__warnings' }, c.warnings.map((w) => h('li', null, status({ tone: 'warning', line: true }, w)))) : null);
}

/** Where the new workloads would go, or why they could not. */
function placement(c, nodes, noneChosen) {
  if (noneChosen) return status({ tone: 'warning', line: true }, t('pages.cluster_deploy.choose_one'));
  if (c.unplaced) return status({ tone: 'critical', line: true }, t('pages.cluster_deploy.unplaced', { why: c.unplaced }));
  const p = Object.entries(c.placements || {});
  if (!p.length) return h('p', { class: 'sub' }, t('pages.cluster_deploy.stays'));
  const byId = new Map(nodes.map((n) => [n.id, n]));
  return h('div', { class: 'row wrap' }, h('span', { class: 'sub' }, t('pages.cluster_deploy.goes')),
    p.map(([w, n]) => h('span', { class: 'fd-deploy__svc' }, h('span', { class: 'mono' }, w), h('span', { class: 'sub' }, ` → ${byId.get(n) ? nodeName(byId.get(n)) : n}`))));
}

/** The first pool address with `port`/`protocol` free: no other stack's, and not taken by a row here already. */
function freeAddress(pool, stack, port, protocol, rows) {
  const taken = (a) => (a.exposures || []).some((e) => e.stack !== stack && e.port === port && e.protocol === protocol)
    || rows.some((r) => r.address === a.address && r.port === port && r.protocol === protocol);
  const a = pool.find((x) => !taken(x));
  return a ? a.address : '';
}

/** A stack's exposures on the pool, edited as rows: service, container port, protocol, then the pool address and the
 *  port outside, or Internal only. `offer(services, suggest)` adds a row for each port a Compose file asks for that
 *  has none yet, on a free pool address when `suggest` (a new stack), else internal only. `read()` is the exposures,
 *  or null when one is incomplete. Used here and by the stacks page's Exposures dialog. */
export function exposureEditor({ pool, stack, initial = [] }) {
  let services = [...new Set(initial.map((e) => e.service))];
  const asked = new Set(initial.map((e) => `${e.service}:${e.container_port}/${e.protocol}`));
  const rows = h('div', { class: 'stack' });
  const field = (label, input, wide) => h('label', { class: ['fui-field', 'fd-expose__field', wide && 'fd-expose__field--wide'] }, h('span', { class: 'fui-field__label' }, label), input);
  const select = (options, value) => h('select', { class: 'fui-field__input' }, options.map((o) => h('option', { value: o.value, selected: o.value === value }, o.label)));
  const svcOptions = () => services.map((s) => ({ value: s, label: s }));
  const row = (e = {}) => {
    const svc = select(svcOptions(), e.service || services[0]);
    const cport = h('input', { class: 'fui-field__input mono', type: 'number', min: 1, max: 65535, value: e.container_port || '' });
    const proto = select([{ value: 'tcp', label: 'tcp' }, { value: 'udp', label: 'udp' }], e.protocol || 'tcp');
    const addr = select([{ value: '', label: t('pages.cluster_deploy.internal_only') }, ...pool.map((p) => ({ value: p.address, label: p.address }))], e.address || '');
    const port = h('input', { class: 'fui-field__input mono', type: 'number', min: 1, max: 65535, value: e.port || '' });
    const paint = () => el.classList.toggle('fd-expose--internal', !addr.value);
    addr.addEventListener('change', paint);
    const el = h('div', { class: 'fd-expose' },
      field(t('pages.cluster_stacks.service'), svc), field(t('pages.cluster_stacks.container_port'), cport), field(t('pages.cluster_stacks.protocol'), proto),
      h('span', { class: 'fd-expose__arrow', 'aria-hidden': 'true' }, '←'),
      field(t('pages.cluster.floating_address'), addr, true), field(t('pages.cluster_stacks.outside_port'), port),
      button({ size: 'sm', variant: 'danger', onClick: () => el.remove() }, t('common.remove')));
    el.read = () => ({ service: svc.value, container_port: Number(cport.value), protocol: proto.value, address: addr.value, port: Number(port.value) });
    el.setServices = () => { const v = svc.value; mount(svc, ...svcOptions().map((o) => h('option', { value: o.value, selected: o.value === v }, o.label))); };
    paint();
    return el;
  };
  const current = () => [...rows.children].map((r) => r.read());
  for (const e of initial) rows.append(row(e));
  const add = button({ size: 'sm', onClick: () => rows.append(row({ address: pool[0] ? pool[0].address : '' })) }, t('pages.cluster_stacks.add_exposure'));
  const el = h('div', { class: 'stack' },
    h('p', null, t('pages.cluster_deploy.reach_text')),
    pool.length ? null : h('p', { class: 'sub' }, t('pages.cluster_stacks.no_pool')),
    rows, h('div', null, add));
  const sync = () => { add.disabled = !services.length; for (const r of rows.children) r.setServices(); };
  sync();
  return {
    el,
    offer(found, suggest) {
      services = Object.keys(found);
      sync();
      for (const [svc, v] of Object.entries(found)) {
        for (const p of v.ports || []) {
          const protocol = p.protocol || 'tcp';
          const key = `${svc}:${p.container_port}/${protocol}`;
          if (asked.has(key)) continue;
          asked.add(key);
          const outside = p.host_port || p.container_port;
          rows.append(row({ service: svc, container_port: p.container_port, protocol, port: outside, address: suggest ? freeAddress(pool, stack, outside, protocol, current()) : '' }));
        }
      }
    },
    read() {
      const list = current().filter((x) => x.address);
      return list.some((x) => !x.container_port || !x.port) ? null : list;
    },
  };
}
