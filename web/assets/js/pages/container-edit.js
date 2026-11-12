// Making a container, editing one (which replaces it: DECISIONS "Containers" 3), or duplicating one. The form edits
// Docker's own create body: the everyday settings as fields, and the whole body as JSON in the second tab, so every
// option Docker has is reachable and nothing the form does not show is lost.

import { accessPicker } from '../ownership.js';
import { h, mount } from '../dom.js';
import { get, post } from '../api.js';
import { base } from '../docker.js';
import { navigate } from '../router.js';
import { postStream, layerTally } from '../stream.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { formField, textArea, inlineError } from '../../finui/components/field/field.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { button } from '../../finui/components/button/button.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { callout } from '../../finui/components/callout/callout.js';
import { meter } from '../../finui/components/meter/meter.js';
import { errorState } from '../../finui/components/error/error.js';
import { sk } from '../../finui/components/skeleton/skeleton.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

/** "sh -c 'echo hi'" as words, the way the server's console reads a command line. */
export function words(s) {
  const out = []; let cur = '', q = null, any = false;
  for (const c of s) {
    if (q) { if (c === q) q = null; else cur += c; }
    else if (c === '"' || c === "'") { q = c; any = true; }
    else if (/\s/.test(c)) { if (any || cur) { out.push(cur); cur = ''; any = false; } }
    else cur += c;
  }
  if (any || cur) out.push(cur);
  return out;
}
const unwords = (a) => (a || []).map((w) => (w === '' || /[\s"']/.test(w) ? `'${w.replace(/'/g, "'\"'\"'")}'` : w)).join(' ');
const lines = (s) => s.split('\n').map((l) => l.trim()).filter((l) => l && !l.startsWith('#'));
const toMap = (s) => Object.fromEntries(lines(s).map((l) => { const i = l.indexOf('='); return i < 0 ? [l, ''] : [l.slice(0, i), l.slice(i + 1)]; }));
const fromMap = (m) => Object.entries(m || {}).map(([k, v]) => `${k}=${v}`).join('\n');

/** A small table of rows the person adds to and takes from: ports, volumes. */
function rowsEditor({ columns, values, add }) {
  const list = h('div', { class: 'rows-edit__list' });
  const rows = [];
  const addRow = (v = {}) => {
    const inputs = {};
    const row = h('div', { class: 'rows-edit__row' }, columns.map((c) => {
      if (c.kind === 'toggle') {
        inputs[c.key] = { checked: !!v[c.key] };
        return h('label', { class: 'row rows-edit__toggle' }, toggle({ checked: !!v[c.key], onChange: (x) => { inputs[c.key].checked = x; } }), h('span', null, c.label));
      }
      if (c.kind === 'select') {
        const s = h('select', { class: 'fui-field__input', 'aria-label': c.label }, c.options.map((o) => h('option', { value: o, selected: (v[c.key] || c.options[0]) === o }, o)));
        inputs[c.key] = s;
        return s;
      }
      const i = h('input', { class: 'fui-field__input', 'aria-label': c.label, placeholder: c.placeholder || c.label, value: v[c.key] || '', spellcheck: false });
      inputs[c.key] = i;
      return i;
    }), button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.container_edit.take_row_away'), onClick: () => { row.remove(); rows.splice(rows.indexOf(entry), 1); } }, icon('x', 14)));
    const entry = { row, inputs };
    rows.push(entry);
    list.append(row);
  };
  (values || []).forEach(addRow);
  return {
    el: h('div', { class: 'rows-edit' }, list, button({ size: 'sm', variant: 'ghost', onClick: () => addRow() }, icon('plus', 13), add)),
    read: () => rows.map(({ inputs }) => Object.fromEntries(Object.entries(inputs).map(([k, i]) => [k, 'checked' in i && !(i instanceof HTMLElement) ? i.checked : i.value.trim()]))),
  };
}

const POLICIES = ['no', 'always', 'unless-stopped', 'on-failure'];

function form(body, nets) {
  const hc = body.HostConfig || {};
  const f = {};
  const field = (id, label, value, opts = {}) => { f[id] = formField({ id, label, ...opts }); f[id].input.value = value ?? ''; return f[id].el; };
  const area = (id, label, value, opts = {}) => { f[id] = textArea({ id, label, rows: 5, ...opts }); f[id].input.value = value ?? ''; f[id].input.classList.add('mono'); return f[id].el; };

  const bindings = hc.PortBindings || {};
  const portRows = Object.entries(bindings).flatMap(([p, bs]) => (bs && bs.length ? bs : [{}]).map((b) => {
    const [port, proto] = p.split('/');
    return { host: b.HostPort || '', container: port, proto: proto || 'tcp', ip: b.HostIp || '' };
  }));
  const ports = rowsEditor({ add: t('pages.container_edit.add_port'), values: portRows, columns: [
    { key: 'host', label: t('pages.container_edit.host_port'), placeholder: '8080' }, { key: 'container', label: t('pages.container_edit.container_port'), placeholder: '80' },
    { key: 'proto', label: t('pages.container_edit.protocol'), kind: 'select', options: ['tcp', 'udp', 'sctp'] }, { key: 'ip', label: t('pages.container_edit.host_address'), placeholder: t('pages.container_edit.all_addresses') }] });
  const binds = (hc.Binds || []).map((b) => { const p = b.split(':'); return { source: p[0], target: p[1] || '', ro: (p[2] || '').split(',').includes('ro') }; });
  const vols = rowsEditor({ add: t('pages.container_edit.add_volume'), values: binds, columns: [
    { key: 'source', label: t('pages.container_edit.volume_or_folder'), placeholder: t('pages.container_edit.volume_placeholder') }, { key: 'target', label: t('pages.container_edit.path_inside'), placeholder: '/data' },
    { key: 'ro', label: t('pages.container_edit.read_only'), kind: 'toggle' }] });

  let policy = (hc.RestartPolicy && hc.RestartPolicy.Name) || 'no';
  const flags = { Privileged: !!hc.Privileged, Init: !!hc.Init, Tty: !!body.Tty };
  const flag = (key, label, help) => settingRow({ id: `flag-${key}`, label, help, control: toggle({ checked: flags[key], labelledby: `flag-${key}-label`, onChange: (v) => { flags[key] = v; } }) });
  const endpoints = Object.keys((body.NetworkingConfig && body.NetworkingConfig.EndpointsConfig) || {});
  const current = endpoints[0] || hc.NetworkMode || 'bridge';
  const netSel = h('select', { class: 'fui-field__input', id: 'network' }, [...new Set([current, 'bridge', 'host', t('common.none_word'), ...nets])].map((n) => h('option', { value: n, selected: n === current }, n)));
  const mounts = (hc.Mounts || []).length;
  // Where a refusal by the host's security settings is shown, by the field the server names ("Access" 16).
  const slots = { binds: h('div'), network_mode: h('div'), privileged: h('div') };

  const el = h('div', { class: 'stack' },
    card({ title: t('pages.container_edit.what_it_runs'), body: h('div', { class: 'form-grid' },
      field('image', t('pages.containers.image'), body.Image, { placeholder: 'nginx:latest', help: t('pages.container_edit.pulled_if_missing') }),
      field('cmd', t('pages.container.command'), unwords(body.Cmd), { placeholder: t('pages.container_edit.images_own') }),
      field('entrypoint', t('pages.container_edit.entrypoint'), unwords(body.Entrypoint), { placeholder: t('pages.container_edit.images_own') }),
      field('workdir', t('pages.container_edit.workdir'), body.WorkingDir, { placeholder: t('pages.container_edit.images_own') }),
      field('user', t('nav.user'), body.User, { placeholder: t('pages.container_edit.images_own') }),
      field('hostname', t('pages.container_edit.hostname'), body.Hostname, { placeholder: t('pages.container_edit.hostname_placeholder') })) }),
    card({ title: t('pages.containers.ports'), sub: t('pages.container_edit.ports_sub'), body: ports.el }),
    card({ title: t('pages.container_edit.volumes_and_folders'), body: h('div', { class: 'stack' }, vols.el, slots.binds, mounts ? callout({ tone: 'info', body: t('pages.container_edit.more_mounts', { count: mounts }) }) : null) }),
    card({ title: t('pages.container_edit.env_and_labels'), body: h('div', { class: 'grid-2' },
      area('env', t('pages.container.environment'), (body.Env || []).join('\n'), { help: t('pages.container_edit.env_help') }),
      area('labels', t('common.labels'), fromMap(body.Labels), { help: t('pages.container_edit.labels_help') })) }),
    card({ title: t('pages.container_edit.network_restarts'), body: h('div', { class: 'stack' },
      h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: 'network' }, t('pages.container.network')), netSel, slots.network_mode),
      settingRow({ id: 'policy', label: t('pages.container_edit.restart'), help: t('pages.container_edit.restart_help'), control: segmented({ label: t('pages.container_edit.restart'), size: 'sm', value: policy, options: POLICIES.map((p) => ({ value: p, label: p })), onChange: (v) => { policy = v; } }) })) }),
    card({ title: t('pages.container_edit.limits'), body: h('div', { class: 'stack' },
      h('div', { class: 'form-grid' },
        field('memory', t('pages.container_edit.memory_mb'), hc.Memory ? Math.round(hc.Memory / 1048576) : '', { inputMode: 'numeric', placeholder: t('common.none_word') }),
        field('cpus', t('pages.dashboard.cpus'), hc.NanoCpus ? hc.NanoCpus / 1e9 : '', { inputMode: 'decimal', placeholder: t('pages.container_edit.all_of_them') })),
      flag('Init', t('pages.container_edit.init'), t('pages.container_edit.init_help')),
      flag('Tty', t('pages.container_edit.interactive'), t('pages.container_edit.interactive_help')),
      flag('Privileged', t('pages.container.privileged'), t('pages.container_edit.privileged_help')), slots.privileged) }));

  /** The body as the form says, on top of what it was (what the form does not show is kept). */
  const read = () => {
    const out = structuredClone(body);
    const hc2 = out.HostConfig = out.HostConfig || {};
    const set = (o, k, v) => { if (v === '' || v == null || (Array.isArray(v) && !v.length)) delete o[k]; else o[k] = v; };
    set(out, 'Image', f.image.input.value.trim());
    set(out, 'Cmd', words(f.cmd.input.value)); set(out, 'Entrypoint', words(f.entrypoint.input.value));
    set(out, 'WorkingDir', f.workdir.input.value.trim()); set(out, 'User', f.user.input.value.trim()); set(out, 'Hostname', f.hostname.input.value.trim());
    set(out, 'Env', lines(f.env.input.value));
    const labels = toMap(f.labels.input.value); set(out, 'Labels', Object.keys(labels).length ? labels : null);
    const pb = {}, ex = {};
    for (const p of ports.read()) {
      if (!p.container) continue;
      const key = `${p.container}/${p.proto || 'tcp'}`;
      ex[key] = {};
      (pb[key] = pb[key] || []).push({ HostIp: p.ip || '', HostPort: p.host || '' });
    }
    set(hc2, 'PortBindings', Object.keys(pb).length ? pb : null);
    out.ExposedPorts = { ...(out.ExposedPorts || {}), ...ex };
    if (!Object.keys(out.ExposedPorts).length) delete out.ExposedPorts;
    set(hc2, 'Binds', vols.read().filter((v) => v.source && v.target).map((v) => `${v.source}:${v.target}${v.ro ? ':ro' : ''}`));
    hc2.RestartPolicy = { Name: policy === 'no' ? '' : policy, MaximumRetryCount: 0 };
    const mem = Number(f.memory.input.value); set(hc2, 'Memory', mem > 0 ? Math.round(mem * 1048576) : null);
    const cpus = Number(f.cpus.input.value); set(hc2, 'NanoCpus', cpus > 0 ? Math.round(cpus * 1e9) : null);
    hc2.Privileged = flags.Privileged; hc2.Init = flags.Init;
    out.Tty = flags.Tty; out.OpenStdin = flags.Tty;
    const net = netSel.value;
    if (net !== current) {
      hc2.NetworkMode = net;
      out.NetworkingConfig = ['host', t('common.none_word'), 'bridge'].includes(net) && net !== 'bridge' ? null : { EndpointsConfig: { [net]: {} } };
      if (!out.NetworkingConfig) delete out.NetworkingConfig;
    }
    return out;
  };
  return { el, read, errors: f, slots };
}

async function pullWithProgress(image, slot) {
  const tally = layerTally();
  const bar = meter({ indeterminate: true, block: true, label: t('pages.container_edit.pulling', { image }), class: 'fm-film' });
  const line = h('p', { class: 'muted mono' }, t('pages.container_edit.pulling_dots', { image }));
  mount(slot, h('div', { class: 'stack' }, line, bar));
  await postStream(base('/images/pull'), { image }, (l) => {
    tally.add(l);
    const p = tally.percent;
    if (p != null) mount(slot, h('div', { class: 'stack' }, h('p', { class: 'muted mono' }, `${l.status || t('pages.container_edit.pulling_word')}${l.progress ? ' ' + l.progress : ''}`), meter({ value: p, block: true, label: t('pages.container_edit.pulling', { image }) })));
    else line.textContent = l.status || line.textContent;
  });
}

/** `/containers/new` (`?from=` to duplicate) and `/containers/:id/edit`. */
export function containerEditPage({ el, params, query, signal }) {
  const editing = params.id || null;
  const from = editing || query.get('from');
  const title = editing ? t('routes.edit', { name: editing }) : from ? t('pages.container_edit.duplicate', { name: from }) : t('routes.add_container');
  const slot = h('div');
  el.append(pageHeader(title, editing ? t('pages.container_edit.replace_sub') : t('pages.container_edit.new_sub')), slot);
  mount(slot, sk.block(320));
  (async () => {
    try {
      const [loaded, nets] = await Promise.all([
        from ? get(base(`/containers/${encodeURIComponent(from)}/config`), { signal }) : Promise.resolve({ name: '', config: { Image: query.get('image') || '', HostConfig: { RestartPolicy: { Name: 'unless-stopped' } } } }),
        get(base('/networks'), { signal }).then((l) => l.filter((n) => !n.built_in).map((n) => n.Name)).catch(() => []),
      ]);
      draw(loaded, nets);
    } catch (e) { if (!signal.aborted) mount(slot, errorState(e, () => location.reload())); }
  })();

  function draw(loaded, nets) {
    let body = loaded.config;
    let mode = 'form';
    const name = formField({ id: 'name', label: t('common.name'), placeholder: t('pages.volumes.name_placeholder'), autocomplete: 'off' });
    name.input.value = editing ? loaded.name : from ? `${loaded.name}-copy` : '';
    let pull = false;
    // Who may see a new one ("Access" 4); an edit keeps what it had.
    const access = editing ? null : accessPicker({ id: 'container-access' });
    let built = form(body, nets);
    const json = textArea({ id: 'json', label: t('pages.container_edit.create_body'), rows: 28, help: t('pages.container_edit.create_body_help') });
    json.input.classList.add('mono');
    const formSlot = h('div', null, built.el);
    const tabsEl = tabs({ label: t('pages.container_edit.how_to_edit'), value: 'form', tabs: [{ key: 'form', label: t('pages.container_edit.form'), panel: formSlot }, { key: 'json', label: 'JSON', panel: json.el }],
      onChange: (k) => {
        if (k === 'json') { body = built.read(); json.input.value = JSON.stringify(body, null, 2); }
        else {
          try { body = JSON.parse(json.input.value); } catch (e) { toast({ text: t('pages.container_edit.bad_json', { message: e.message }), tone: 'critical' }); }
          built = form(body, nets); mount(formSlot, built.el);
        }
        mode = k;
      } });
    const progress = h('div');
    const problem = h('div');
    const save = button({ variant: 'primary', type: 'submit' }, editing ? t('pages.container_edit.replace') : t('pages.container_edit.create_and_start'));
    const page = h('form', { class: 'stack', novalidate: true },
      card({ body: h('div', { class: 'stack' }, name.el, access ? access.el : null, settingRow({ id: 'pull', label: t('pages.containers.pull_first'), help: t('pages.containers.pull_first_help'), control: toggle({ checked: false, labelledby: 'pull-label', onChange: (v) => { pull = v; } }) })) }),
      tabsEl, problem, progress,
      h('div', { class: 'row row--end' }, button({ href: editing ? `/containers/${encodeURIComponent(editing)}` : '/containers' }, t('common.cancel')), save));
    page.addEventListener('submit', async (e) => {
      e.preventDefault();
      name.setError(''); problem.replaceChildren(); Object.values(built.slots).forEach((x) => x.replaceChildren());
      let config;
      try { config = mode === 'json' ? JSON.parse(json.input.value) : built.read(); } catch (err) { problem.replaceChildren(callout({ tone: 'critical', body: t('pages.container_edit.bad_json', { message: err.message }) })); return; }
      if (!config.Image) { built.errors.image && built.errors.image.setError(t('pages.container_edit.say_image')); return; }
      save.disabled = true;
      try {
        const here = await get(base(`/images/${encodeURIComponent(config.Image)}`)).then(() => true, () => false);
        if (pull || !here) await pullWithProgress(config.Image, progress);
        mount(progress, h('p', { class: 'muted' }, editing ? t('pages.container_edit.replacing') : t('pages.container_edit.creating')));
        const made = await post(base('/containers'), { name: name.input.value.trim() || null, config, pull: false, start: true, replace: editing ? loaded.name : null, access: access ? access.value() : undefined });
        toast({ text: editing ? t('pages.container_edit.replaced', { name: loaded.name }) : t('pages.container_edit.runs'), tone: 'good' });
        navigate(`/containers/${encodeURIComponent(name.input.value.trim() || made.id)}`);
      } catch (err) {
        progress.replaceChildren();
        const slot = mode === 'form' && built.slots[err.field];
        if (err.field === 'name') name.setError(err.message);
        else if (slot) { slot.replaceChildren(inlineError(`${err.field}-error`, err.message)); slot.scrollIntoView({ block: 'center' }); }
        else problem.replaceChildren(callout({ tone: 'critical', title: editing ? t('pages.container_edit.not_replaced') : t('pages.container_edit.not_created'), body: err.message }));
      } finally { save.disabled = false; }
    });
    mount(slot, page);
  }
}
