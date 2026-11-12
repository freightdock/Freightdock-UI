// Swarm services: the list with each one's tasks running against those wanted; one service with its tasks, its logs
// (every task's together, followed live) and its spec; making one from a few fields or Docker's whole spec as JSON;
// scaling, updating, rolling back and removing, for an administrator.

import { h, ago, when, mount, num, bytes } from '../dom.js';
import { get, post, put, del, socket, qs } from '../api.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { navigate } from '../router.js';
import { confirm } from '../confirm.js';
import { sbase, swarmPage, replicas, serviceImage, servicePorts, stackOf, nodeName } from '../swarm.js';
import { tasksTable } from './swarm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { button } from '../../finui/components/button/button.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { formField, textArea } from '../../finui/components/field/field.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { callout } from '../../finui/components/callout/callout.js';
import { codeBlock } from '../../finui/components/code-block/code-block.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { outputView } from '../components/output-view/output-view.js';
import { t, tOr } from '../i18n.js';

const lines = (s) => s.split('\n').map((l) => l.trim()).filter(Boolean);
/** A command line as words: spaces part them, quotes keep a word whole. */
function words(s) {
  const out = []; let cur = '', q = null, any = false;
  for (const c of s) {
    if (q) { if (c === q) q = null; else cur += c; continue; }
    if (c === '"' || c === "'") { q = c; any = true; continue; }
    if (/\s/.test(c)) { if (cur || any) out.push(cur); cur = ''; any = false; continue; }
    cur += c;
  }
  if (cur || any) out.push(cur);
  return out;
}
/** "8080:80" or "8080:80/udp" or "80" (Docker picks the published port) as an endpoint port. */
function port(s) {
  const m = /^(?:(\d+):)?(\d+)(?:\/(tcp|udp|sctp))?$/.exec(s);
  if (!m) throw new Error(t('pages.swarm_services.bad_port', { port: s }));
  return { Protocol: m[3] || 'tcp', TargetPort: Number(m[2]), PublishedPort: m[1] ? Number(m[1]) : undefined, PublishMode: 'ingress' };
}

/** The spec the few fields make. */
function specOf(f) {
  const container = { Image: f.image };
  const cmd = words(f.command);
  if (cmd.length) container.Args = cmd;
  if (f.env.length) container.Env = f.env;
  const spec = { Name: f.name, TaskTemplate: { ContainerSpec: container, RestartPolicy: { Condition: 'any' } }, Mode: f.global ? { Global: {} } : { Replicated: { Replicas: f.replicas } } };
  if (f.constraints.length) spec.TaskTemplate.Placement = { Constraints: f.constraints };
  if (f.ports.length) spec.EndpointSpec = { Ports: f.ports.map(port) };
  if (f.networks.length) spec.TaskTemplate.Networks = f.networks.map((n) => ({ Target: n }));
  return spec;
}

function createDialog(after) {
  const name = formField({ id: 'svc-name', label: t('common.name'), autocomplete: 'off' });
  const image = formField({ id: 'svc-image', label: t('pages.containers.image'), placeholder: 'nginx:alpine', autocomplete: 'off' });
  const count = formField({ id: 'svc-replicas', label: t('pages.swarm_services.replicas'), type: 'number', inputMode: 'numeric' });
  count.input.value = '1';
  const command = formField({ id: 'svc-cmd', label: t('pages.container.command'), placeholder: t('pages.container_edit.images_own'), help: t('pages.swarm_services.command_help') });
  const ports = textArea({ id: 'svc-ports', label: t('pages.containers.ports'), rows: 2, help: t('pages.swarm_services.ports_help') });
  const env = textArea({ id: 'svc-env', label: t('pages.container.environment'), rows: 3, help: t('pages.container_edit.env_help') });
  const networks = textArea({ id: 'svc-nets', label: t('nav.networks'), rows: 2, help: t('pages.swarm_services.networks_help') });
  const constraints = textArea({ id: 'svc-place', label: t('pages.swarm_services.placement'), rows: 2, help: t('pages.swarm_services.placement_help') });
  let global = false;
  const json = textArea({ id: 'svc-json', label: t('pages.swarm_services.spec_label'), rows: 16, help: t('pages.swarm_services.spec_help') });
  json.input.classList.add('mono');
  const fields = () => ({ name: name.input.value.trim(), image: image.input.value.trim(), replicas: Math.max(0, Number(count.input.value) || 0), global, command: command.input.value,
    ports: lines(ports.input.value), env: lines(env.input.value), networks: lines(networks.input.value), constraints: lines(constraints.input.value) });
  let mode = 'fields';
  const simple = h('div', { class: 'stack' }, name.el, image.el,
    settingRow({ id: 'svc-global', label: t('pages.swarm_services.every_node'), help: t('pages.swarm_services.every_node_help'), control: toggle({ checked: false, labelledby: 'svc-global-label', onChange: (v) => { global = v; count.el.hidden = v; } }) }),
    count.el, command.el, ports.el, env.el, networks.el, constraints.el);
  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  const tabsEl = tabs({ label: t('pages.swarm_services.made_from'), value: mode, onChange: (k) => {
    if (k === 'json' && mode === 'fields') { try { json.input.value = JSON.stringify(specOf(fields()), null, 2); } catch (x) { mount(err, x.message); } }
    mode = k;
  }, tabs: [{ key: 'fields', label: t('pages.swarm_services.fields'), panel: simple }, { key: 'json', label: t('pages.swarm_services.as_json'), panel: json.el }] });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.create'));
  const form = h('form', { class: 'stack', novalidate: true }, tabsEl, err, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.swarm_services.create_title'), body: form, initialFocus: name.input, wide: true });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); mount(err); name.setError(''); image.setError('');
    let spec;
    try { spec = mode === 'json' ? JSON.parse(json.input.value) : specOf(fields()); } catch (x) { mount(err, x.message); return; }
    go.disabled = true;
    try { await post(sbase('/services'), spec); m.close(); toast({ text: t('pages.swarm_services.made', { name: spec.Name }), tone: 'good' }); after(spec.Name); }
    catch (x) { if (mode === 'fields' && x.field === 'name') name.setError(x.message); else if (mode === 'fields' && x.field === 'image') image.setError(x.message); else mount(err, x.message); }
    finally { go.disabled = false; }
  });
}

export const servicesPage = swarmPage(({ el, signal, every }) => {
  let words_ = [];
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(sbase('/services'), { signal: s }),
    draw: (list) => table({
      rows: list.filter((s) => matches(words_, s.Spec.Name, serviceImage(s), stackOf(s))), filter: false, empty: [t('pages.stack.no_services'), t('pages.swarm.no_services_text')],
      columns: [
        { label: t('common.name'), cell: (s) => h('div', { class: 'name-cell' }, h('a', { href: `/swarm/services/${encodeURIComponent(s.Spec.Name)}` }, s.Spec.Name), stackOf(s) ? h('span', { class: 'sub' }, t('common.stack_name', { name: stackOf(s) })) : null) },
        { label: t('pages.swarm.tasks'), cell: (s) => replicas(s), sort: (s) => (s.ServiceStatus || {}).RunningTasks || 0 },
        { label: t('pages.containers.image'), cell: (s) => h('span', { class: 'mono truncate' }, serviceImage(s)) },
        { label: t('pages.containers.ports'), cell: (s) => h('span', { class: 'mono' }, servicePorts(s)) },
        { label: t('pages.swarm_services.updated'), cell: (s) => h('span', { class: 'nowrap' }, ago(Date.parse(s.UpdatedAt) / 1000)), sort: (s) => Date.parse(s.UpdatedAt) },
      ],
    }),
  });
  every(5000, reload);
  const actions = can('administer') ? button({ size: 'sm', variant: 'primary', onClick: () => createDialog(() => reload()) }, icon('plus', 14), t('pages.swarm_services.create')) : null;
  el.append(pageHeader(t('nav.services'), t('pages.swarm_services.sub'), actions),
    h('div', { class: 'toolbar' }, filterBox(t('pages.swarm_services.filter'), (w) => { words_ = w; reload.redraw(); })), slot);
});

function scaleDialog(s, after) {
  const n = formField({ id: 'scale-n', label: t('pages.swarm_services.replicas'), type: 'number', inputMode: 'numeric' });
  n.input.value = String(((s.Spec.Mode || {}).Replicated || {}).Replicas ?? 1);
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.swarm_services.scale'));
  const form = h('form', { class: 'stack', novalidate: true }, n.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.swarm_services.scale_title', { name: s.Spec.Name }), body: form, initialFocus: n.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); n.setError('');
    try { await post(sbase(`/services/${encodeURIComponent(s.ID)}/scale`), { replicas: Math.max(0, Number(n.input.value) || 0) }); m.close(); toast({ text: t('pages.swarm_services.scaling'), tone: 'good' }); after(); }
    catch (x) { n.setError(x.message); }
  });
}

function imageDialog(s, after) {
  const img = formField({ id: 'svc-new-image', label: t('pages.containers.image'), autocomplete: 'off', help: t('pages.swarm_services.image_help') });
  img.input.value = serviceImage(s);
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.stack.update'));
  const form = h('form', { class: 'stack', novalidate: true }, img.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.swarm_services.update_title', { name: s.Spec.Name }), body: form, initialFocus: img.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); img.setError('');
    const spec = structuredClone(s.Spec);
    spec.TaskTemplate.ContainerSpec.Image = img.input.value.trim();
    spec.TaskTemplate.ForceUpdate = (spec.TaskTemplate.ForceUpdate || 0) + 1;
    try { await put(sbase(`/services/${encodeURIComponent(s.ID)}`), spec); m.close(); toast({ text: t('pages.swarm_services.updating'), tone: 'good' }); after(); }
    catch (x) { img.setError(x.message); }
  });
}

function header(s, reload) {
  const global = !!(s.Spec.Mode || {}).Global;
  const actions = can('administer') ? h('div', { class: 'row' },
    !global ? button({ size: 'sm', onClick: () => scaleDialog(s, reload) }, icon('sliders', 13), t('pages.swarm_services.scale')) : null,
    button({ size: 'sm', onClick: () => imageDialog(s, reload) }, icon('refresh', 13), t('pages.stack.update')),
    dropdownMenu({ label: t('common.more'), size: 'sm', items: [
      { label: t('pages.swarm_services.restart_tasks'), icon: 'repeat', onSelect: async () => {
        const spec = structuredClone(s.Spec);
        spec.TaskTemplate.ForceUpdate = (spec.TaskTemplate.ForceUpdate || 0) + 1;
        try { await put(sbase(`/services/${encodeURIComponent(s.ID)}`), spec); toast({ text: t('pages.swarm_services.replaced'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
      } },
      s.PreviousSpec ? { label: t('pages.swarm_services.roll_back'), icon: 'undo', onSelect: async () => {
        if (!(await confirm({ title: t('pages.swarm_services.roll_back_title', { name: s.Spec.Name }), body: t('pages.swarm_services.roll_back_body'), action: t('pages.swarm_services.roll_back'), danger: false }))) return;
        try { await post(sbase(`/services/${encodeURIComponent(s.ID)}/rollback`), {}); toast({ text: t('pages.swarm_services.rolling_back'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
      } } : null,
      { separator: true },
      { label: t('common.remove'), icon: 'trash', danger: true, onSelect: async () => {
        if (!(await confirm({ title: t('common.remove_name', { name: s.Spec.Name }), body: t('pages.swarm_services.remove_body') }))) return;
        try { await del(sbase(`/services/${encodeURIComponent(s.ID)}`)); toast({ text: t('common.removed'), tone: 'good' }); navigate('/swarm/services'); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
      } },
    ] })) : null;
  return pageHeader(h('span', { class: 'row' }, s.Spec.Name, replicas(s, { bare: true })), serviceImage(s), actions);
}

function overview(s, tasks) {
  const spec = s.Spec, c = (spec.TaskTemplate || {}).ContainerSpec || {}, u = s.UpdateStatus;
  const mode = spec.Mode || {};
  const res = ((spec.TaskTemplate || {}).Resources || {}).Limits || {};
  const running = tasks.filter((task) => (task.Status || {}).State === 'running').length;
  return h('div', { class: 'stack' },
    u && /paused|rollback_paused/.test(u.State) ? callout({ tone: 'critical', title: t(u.State === 'paused' ? 'pages.swarm_services.update_paused' : 'pages.swarm_services.rollback_paused'), body: u.Message || '' }) : null,
    u && /updating|rollback_started/.test(u.State) ? callout({ tone: 'info', body: u.Message || t('pages.swarm_services.under_way') }) : null,
    card({ title: t('pages.stack.service'), body: facts([
      [t('pages.containers.image'), serviceImage(s), { mono: true, wide: true }],
      [t('pages.swarm_services.mode'), mode.Global ? t('pages.swarm_services.mode_global') : mode.Replicated ? t('pages.swarm_services.mode_replicated', { count: mode.Replicated.Replicas }) : t('swarm.mode.job')],
      [t('pages.containers.running'), t('pages.swarm_services.n_tasks', { count: running })], [t('routes.stack'), stackOf(s)],
      [t('pages.containers.ports'), servicePorts(s) || t('pages.swarm_services.none_published'), { mono: true }],
      [t('pages.container.command'), [...(c.Command || []), ...(c.Args || [])].join(' ') || t('pages.container_edit.images_own'), { mono: true, wide: true }],
      [t('pages.container.environment'), (c.Env || []).join('\n') || t('common.none_word'), { mono: true, wide: true }],
      [t('pages.swarm_services.placement'), (((spec.TaskTemplate || {}).Placement || {}).Constraints || []).join(', ') || t('pages.swarm_services.any_node')],
      [t('pages.swarm_services.limits'), [res.NanoCPUs ? t('pages.swarm_services.n_cpus', { n: num(res.NanoCPUs / 1e9, res.NanoCPUs % 1e9 ? 2 : 0) }) : null, res.MemoryBytes ? bytes(res.MemoryBytes) : null].filter(Boolean).join(', ') || t('common.none_word')],
      [t('nav.secrets'), (c.Secrets || []).map((x) => x.SecretName).join(', ') || t('common.none_word')], [t('nav.configs'), (c.Configs || []).map((x) => x.ConfigName).join(', ') || t('common.none_word')],
      [t('common.made'), when(Date.parse(s.CreatedAt) / 1000)], [t('pages.swarm_services.updated'), when(Date.parse(s.UpdatedAt) / 1000)],
      [t('pages.swarm_services.last_update'), u ? `${tOr(`pages.swarm_services.update_state.${u.State}`, u.State)}${u.CompletedAt ? ', ' + ago(Date.parse(u.CompletedAt) / 1000) : ''}` : t('common.none_word')],
      ['Id', s.ID.slice(0, 12), { mono: true }],
    ]) }));
}

function logsTab(s, cleanup) {
  let tail = '500', stamps = false, ws = null;
  const view = outputView({ label: t('pages.containers.logs') });
  const connect = () => {
    if (ws) ws.close();
    view.clear();
    ws = socket(sbase(`/services/${encodeURIComponent(s.ID)}/logs/ws${qs({ tail, timestamps: stamps ? 1 : '' })}`));
    ws.onmessage = (e) => {
      let m; try { m = JSON.parse(e.data); } catch { return; }
      if (m.t != null) view.append(m.s, m.t);
      else if (m.end) { view.end(); view.note(t('pages.swarm_services.logs_end')); }
      else if (m.error) view.note(m.error);
    };
  };
  cleanup(() => ws && ws.close());
  const search = h('input', { class: 'fui-field__input fui-field__input--search', type: 'search', placeholder: t('pages.container.only_lines_with'), 'aria-label': t('pages.container.filter_lines'), autocomplete: 'off' });
  search.addEventListener('input', () => view.setFilter(search.value));
  const tools = h('div', { class: 'toolbar' },
    segmented({ label: t('pages.container.lines'), size: 'sm', value: tail, options: [{ value: '100', label: '100' }, { value: '500', label: '500' }, { value: '5000', label: '5000' }, { value: 'all', label: t('pages.containers.all') }], onChange: (v) => { tail = v; connect(); } }),
    h('label', { class: 'row' }, toggle({ checked: false, labelledby: 'svc-ts-label', onChange: (v) => { stamps = v; connect(); } }), h('span', { id: 'svc-ts-label' }, t('pages.container.times'))),
    h('div', { class: 'fui-field__search grow' }, icon('search', 14), search),
    button({ size: 'sm', onClick: () => view.follow(true) }, icon('arrowDown', 13), t('pages.container.to_the_end')));
  connect();
  return h('div', { class: 'stack' }, h('p', { class: 'muted' }, t('pages.swarm_services.every_task')), tools, view.el);
}

function specTab(s, reload) {
  const text = JSON.stringify(s.Spec, null, 2);
  if (!can('administer')) return codeBlock({ panes: [{ key: 'json', label: t('pages.swarm_services.spec'), text }], label: t('pages.swarm_services.the_spec') });
  const area = textArea({ id: 'svc-spec', label: t('pages.swarm_services.the_spec'), rows: 24, help: t('pages.swarm_services.spec_edit_help') });
  area.input.value = text;
  area.input.classList.add('mono');
  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.swarm_services.update_service'));
  const form = h('form', { class: 'stack', novalidate: true }, area.el, err, h('div', { class: 'row row--end' }, go));
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); mount(err);
    let spec; try { spec = JSON.parse(area.input.value); } catch (x) { mount(err, t('pages.swarm_services.not_json', { message: x.message })); return; }
    go.disabled = true;
    try { await put(sbase(`/services/${encodeURIComponent(s.ID)}`), spec); toast({ text: t('pages.swarm_services.updating'), tone: 'good' }); reload(); }
    catch (x) { mount(err, x.message); } finally { go.disabled = false; }
  });
  return form;
}

export const servicePage = swarmPage(({ el, params, query, signal, cleanup, every }) => {
  const slot = h('div');
  el.append(slot);
  let tab = query.get('tab') || 'overview';
  let ends = [];
  const endAll = () => { ends.forEach((f) => { try { f(); } catch { /* gone */ } }); ends = []; };
  cleanup(endAll);
  let nodes = {};
  const reload = loadInto(slot, {
    signal,
    load: async (s) => {
      const [d, list] = await Promise.all([get(sbase(`/services/${encodeURIComponent(params.id)}`), { signal: s }), get(sbase('/nodes'), { signal: s }).catch(() => [])]);
      nodes = Object.fromEntries(list.map((n) => [n.ID, nodeName(n)]));
      return d;
    },
    draw: ({ service: s, tasks }) => {
      endAll();
      const tabsEl = tabs({ label: t('pages.stack.service'), value: tab, onChange: (k) => { tab = k; history.replaceState(null, '', k === 'overview' ? location.pathname : `?tab=${k}`); }, tabs: [
        { key: 'overview', label: t('pages.container.overview'), panel: () => overview(s, tasks) },
        { key: 'tasks', label: t('pages.swarm.tasks'), count: tasks.filter((x) => x.DesiredState === 'running').length, panel: () => tasksTable(tasks, { services: { [s.ID]: s.Spec.Name }, nodes }) },
        { key: 'logs', label: t('pages.containers.logs'), panel: () => logsTab(s, (fn) => ends.push(fn)) },
        { key: 'spec', label: t('pages.swarm_services.spec'), panel: () => specTab(s, reload) },
        { key: 'inspect', label: t('pages.container.inspect'), panel: () => codeBlock({ panes: [{ key: 'json', label: 'JSON', text: JSON.stringify(s, null, 2) }], label: t('pages.container.dockers_inspect') }) },
      ] });
      return h('div', { class: 'stack' }, header(s, reload), tabsEl);
    },
  });
  // Refreshed only while the overview or the tasks are shown: a redraw would cut the logs and lose an edit to the spec.
  every(5000, () => { if (tab === 'overview' || tab === 'tasks') reload(); });
});
