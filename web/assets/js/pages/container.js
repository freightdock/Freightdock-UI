// One container: its actions in the header, then Overview (what it is and how it is set up), Logs (followed live),
// Console (a shell inside it, or attached to its main process), Stats (live charts), History (charts of what it used over
// hours or days, as Freightdock sampled it), Processes and Inspect (Docker's own answer, whole).

import { accessCard } from '../ownership.js';
import { h, bytes, when, ago, shortId, mount, num, pct, clock, day, span } from '../dom.js';
import { get, post, put, socket, qs } from '../api.js';
import { base, stateBadge, projectOf, updateBadge } from '../docker.js';
import { status } from '../../finui/components/badge/badge.js';
import { loadInto, table } from '../view.js';
import { can, STATS_EVERY, statsEvery, setStatsEvery } from '../state.js';
import { navigate } from '../router.js';
import { confirm } from '../confirm.js';
import { act, removeContainer, recreate } from './containers.js';
import { renameContainer } from './rename.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { button } from '../../finui/components/button/button.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { formField } from '../../finui/components/field/field.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { toast } from '../../finui/components/toast/toast.js';
import { lineChart } from '../../finui/components/line-chart/line-chart.js';
import { statTile } from '../../finui/components/stat-tile/stat-tile.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { callout } from '../../finui/components/callout/callout.js';
import { codeBlock } from '../../finui/components/code-block/code-block.js';
import { chip, chipSet } from '../../finui/components/chip/chip.js';
import { icon } from '../../finui/core.js';
import { outputView } from '../components/output-view/output-view.js';
import { terminal } from '../components/terminal/terminal.js';
import { webhookCard } from '../webhook.js';
import { t, tOr, lang } from '../i18n.js';

const pairs = (obj) => Object.entries(obj || {}).sort(([a], [b]) => a.localeCompare(b, lang()));

function kv(rows, empty) {
  if (!rows.length) return h('p', { class: 'muted' }, empty);
  return h('dl', { class: 'kv' }, rows.map(([k, v]) => [h('dt', { class: 'mono' }, k), h('dd', { class: 'mono' }, v)]));
}

// A container on the host's network, on none, or sharing another's has no networks of its own to join or leave.
const ownNetworks = (hc) => !['host', 'none'].includes(hc.NetworkMode) && !String(hc.NetworkMode || '').startsWith('container:');

function connectDialog(c, after) {
  const joined = new Set(Object.keys((c.NetworkSettings || {}).Networks || {}));
  const name = (c.Name || '').replace(/^\//, '');
  const net = h('select', { class: 'fui-field__input', id: 'join-net', 'aria-label': t('pages.container.network') }, h('option', { value: '' }, t('common.loading')));
  const alias = formField({ id: 'join-alias', label: t('pages.networks.aliases'), placeholder: t('pages.networks.aliases_placeholder'), autocomplete: 'off' });
  const ip = formField({ id: 'join-ip', label: t('pages.container.ipv4'), placeholder: t('pages.container.docker_chooses'), autocomplete: 'off' });
  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  const go = button({ variant: 'primary', type: 'submit', disabled: true }, t('pages.networks.connect'));
  const form = h('form', { class: 'stack', novalidate: true },
    h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', for: 'join-net' }, t('pages.container.network')), net), alias.el, ip.el, err,
    h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.container.connect_title', { name }), body: form, initialFocus: net });
  get(base('/networks')).then((list) => {
    const open = list.filter((n) => !joined.has(n.Name) && !['host', 'none'].includes(n.Name)).sort((a, b) => a.Name.localeCompare(b.Name, lang()));
    mount(net, ...(open.length ? open.map((n) => h('option', { value: n.Id }, `${n.Name} (${n.Driver})`)) : [h('option', { value: '' }, t('pages.container.on_every_network'))]));
    go.disabled = !open.length;
  }).catch((e) => { mount(err, e.message); });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); mount(err); ip.setError('');
    if (!net.value) return;
    go.disabled = true;
    try {
      await post(base(`/networks/${encodeURIComponent(net.value)}/connect`), { container: c.Id, aliases: alias.input.value.split(',').map((x) => x.trim()).filter(Boolean), ipv4: ip.input.value.trim() || null });
      m.close(); toast({ text: t('pages.container.connected', { name }), tone: 'good' }); after();
    } catch (x) { mount(err, x.message); go.disabled = false; }
  });
}

async function disconnect(c, network, after) {
  const name = (c.Name || '').replace(/^\//, '');
  if (!(await confirm({ title: t('pages.networks.disconnect_title', { container: name, network }), body: t('pages.container.disconnect_body'), action: t('pages.networks.disconnect') }))) return;
  try { await post(base(`/networks/${encodeURIComponent(network)}/disconnect`), { container: c.Id, force: false }); toast({ text: t('pages.container.disconnected', { name, network }), tone: 'good' }); after(); }
  catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

function networksCard(c, reload) {
  const hc = c.HostConfig || {}, net = c.NetworkSettings || {};
  const may = can('manage') && ownNetworks(hc);
  const rows = pairs(net.Networks);
  const info = (e) => [e.IPAddress, e.Aliases && e.Aliases.length ? t('pages.container.aliases', { list: e.Aliases.join(', ') }) : null].filter(Boolean).join(' · ') || t('pages.container.no_address');
  const body = !rows.length ? h('p', { class: 'muted' }, ownNetworks(hc) ? t('pages.container.on_no_network') : t(hc.NetworkMode === 'none' ? 'pages.container.uses_none' : hc.NetworkMode === 'host' ? 'pages.container.uses_host' : 'pages.container.uses_other'))
    : may ? h('ul', { class: 'plain-list' }, rows.map(([n, e]) => h('li', { class: 'row' },
      h('span', { class: 'mono grow' }, n), h('span', { class: 'mono faint' }, info(e)),
      button({ size: 'sm', variant: 'ghost', onClick: () => disconnect(c, n, reload) }, t('pages.networks.disconnect')))))
      : kv(rows.map(([n, e]) => [n, info(e)]), t('pages.container.on_no_network'));
  const actions = may ? button({ size: 'sm', onClick: () => connectDialog(c, reload) }, icon('plus', 13), t('pages.networks.connect')) : null;
  return card({ title: t('nav.networks'), actions, body });
}

const LABEL = 'freightdock.auto-update';
const VERDICT = { current: t('pages.container.verdict.current'), outdated: t('pages.container.verdict.outdated'), unknown: t('pages.container.verdict.unknown'), pinned: t('pages.container.verdict.pinned'), error: t('pages.container.verdict.error') };

/** Its image against its registry ("Updates"): the last verdict, Update now, and the switch for automatic updates. */
function updatesCard(c, reload) {
  const slot = h('div');
  const name = (c.Name || '').replace(/^\//, '');
  const labelled = ['true', '1', 'yes', 'on'].includes(String(((c.Config || {}).Labels || {})[LABEL] || '').trim().toLowerCase());
  const draw = (u) => {
    const v = (u.containers || {})[c.Id];
    const auto = labelled || (u.opted || []).includes(name);
    const verdict = v ? [updateBadge(v) || status({ tone: v.status === 'current' ? 'good' : 'info', line: true }, tOr(`pages.container.update_status.${v.status}`, v.status)), h('span', { class: 'muted' }, v.detail || VERDICT[v.status] || ''), h('span', { class: 'faint' }, t('pages.container.checked', { when: ago(v.checked_at) }))]
      : [h('span', { class: 'muted' }, u.enabled ? t('pages.container.not_checked_yet') : t('pages.container.check_off'))];
    const go = can('manage') ? button({ size: 'sm', variant: v && v.status === 'outdated' ? 'primary' : null, onClick: async () => {
      go.disabled = true;
      toast({ text: t('pages.container.updating', { name }) });
      try {
        const r = await post(base(`/containers/${encodeURIComponent(c.Id)}/update`));
        toast({ text: r.updated ? t('pages.container.updated', { name }) : t('pages.container.newest', { name }), tone: 'good' });
        if (r.updated) navigate(`/containers/${encodeURIComponent(name)}`, { replace: true }); else reload();
      } catch (e) { toast({ text: t('common.name_said', { name, message: e.message }), tone: 'critical' }); go.disabled = false; }
    } }, icon('download', 13), t('pages.container.update_now')) : null;
    const sw = can('manage') && !labelled ? toggle({ checked: auto, labelledby: 'auto-update-label', onChange: async (on, revert) => {
      try { await put(base(`/containers/${encodeURIComponent(c.Id)}/auto-update`), { enabled: on }); toast({ text: on ? t('pages.container.auto_on') : t('pages.container.auto_off'), tone: 'good' }); }
      catch (e) { revert(!on); toast({ text: e.message, tone: 'critical' }); }
    } }) : null;
    return h('div', { class: 'stack' }, h('div', { class: 'row' }, ...verdict),
      h('div', { class: 'row' }, h('span', { id: 'auto-update-label', class: 'grow' }, labelled ? t('pages.container.auto_by_label', { label: LABEL }) : u.auto_update ? t('pages.container.auto_switch') : t('pages.container.auto_off_everywhere')), sw, go));
  };
  loadInto(slot, { signal: new AbortController().signal, load: (s) => get(base('/updates'), { signal: s }), draw, skeleton: () => h('p', { class: 'muted' }, '…') });
  return card({ title: t('pages.container.image_updates'), body: slot });
}

function overview(c, reload) {
  const cfg = c.Config || {}, hc = c.HostConfig || {}, st = c.State || {}, net = c.NetworkSettings || {};
  const restart = hc.RestartPolicy && hc.RestartPolicy.Name ? hc.RestartPolicy.Name + (hc.RestartPolicy.MaximumRetryCount ? ` (${hc.RestartPolicy.MaximumRetryCount})` : '') : t('common.no');
  const ports = pairs(net.Ports).map(([p, binds]) => [p, (binds || []).map((b) => `${b.HostIp || '0.0.0.0'}:${b.HostPort}`).join(', ') || t('pages.container.not_published')]);
  const env = (cfg.Env || []).map((e) => { const i = e.indexOf('='); return [e.slice(0, i), e.slice(i + 1)]; });
  const mounts = (c.Mounts || []).map((m) => [m.Destination, `${m.Type === 'volume' ? m.Name : m.Source}${m.RW === false ? ' ' + t('pages.container.read_only') : ''}`]);
  return h('div', { class: 'stack' },
    st.Health && st.Health.Status === 'unhealthy' ? callout({ tone: 'critical', title: t('pages.container.unhealthy'), body: ((st.Health.Log || []).slice(-1)[0] || {}).Output || t('pages.container.health_fails') }) : null,
    st.OOMKilled ? callout({ tone: 'warning', body: t('pages.container.oom') }) : null,
    card({ title: t('pages.container.container'), body: facts([
      [t('pages.containers.image'), cfg.Image, { mono: true }], ['Id', shortId(c.Id), { mono: true }], [t('common.made'), when(Date.parse(c.Created) / 1000)],
      [t('pages.container.started'), st.StartedAt && !st.StartedAt.startsWith('0001') ? `${when(Date.parse(st.StartedAt) / 1000)}` : t('common.never')],
      [t('pages.container.restart_policy'), restart], [t('pages.container.restarts'), String(c.RestartCount || 0)],
      [t('pages.container.exit_code'), st.Running ? null : String(st.ExitCode)], [t('routes.stack'), projectOf(c)],
      [t('pages.container.command'), [...(cfg.Entrypoint || []), ...(cfg.Cmd || [])].map((w) => (w === '' || /[\s"']/.test(w) ? `'${w}'` : w)).join(' '), { wide: true, mono: true }],
      [t('pages.container.memory_limit'), hc.Memory ? bytes(hc.Memory) : t('common.none_word')], [t('pages.dashboard.cpus'), hc.NanoCpus ? num(hc.NanoCpus / 1e9, hc.NanoCpus % 1e9 ? 2 : 0) : t('common.all_word')],
      [t('pages.container.privileged'), hc.Privileged ? t('common.yes') : t('common.no')], [t('pages.container.network_mode'), hc.NetworkMode],
      [t('pages.container.size'), c.SizeRw != null ? t('pages.container.size_value', { written: bytes(c.SizeRw), all: bytes(c.SizeRootFs || 0) }) : null],
    ]) }),
    updatesCard(c, reload),
    accessCard({ path: base(`/containers/${encodeURIComponent(c.Id)}/access`), current: c.freightdock_access, reload }),
    h('div', { class: 'grid-2' },
      card({ title: t('pages.containers.ports'), body: kv(ports, t('pages.container.no_ports')) }),
      networksCard(c, reload)),
    card({ title: t('pages.container.mounts'), body: kv(mounts, t('pages.container.nothing_mounted')) }),
    card({ title: t('pages.container.environment'), sub: t('pages.container.variables', { count: env.length }), body: kv(env, t('common.none')) }),
    card({ title: t('common.labels'), body: kv(pairs(cfg.Labels), t('common.none')) }),
    projectOf(c) ? null : webhookCard({ kind: 'container', target: (c.Name || '').replace(/^\//, ''), path: `/containers/${encodeURIComponent((c.Name || '').replace(/^\//, ''))}`,
      what: t('pages.container.webhook_what') }));
}

function logsTab(id, cleanup) {
  let tail = '500', stamps = false, ws = null;
  const view = outputView({ label: t('pages.containers.logs') });
  const connect = () => {
    if (ws) ws.close();
    view.clear();
    ws = socket(base(`/containers/${encodeURIComponent(id)}/logs/ws${qs({ tail, timestamps: stamps ? 1 : '' })}`));
    ws.onmessage = (e) => {
      let m; try { m = JSON.parse(e.data); } catch { return; }
      if (m.t != null) view.append(m.s, m.t);
      else if (m.end) { view.end(); view.note(t('pages.container.stopped_logs_end')); }
      else if (m.error) view.note(m.error);
    };
  };
  cleanup(() => ws && ws.close());
  const search = h('input', { class: 'fui-field__input fui-field__input--search', type: 'search', placeholder: t('pages.container.only_lines_with'), 'aria-label': t('pages.container.filter_lines'), autocomplete: 'off' });
  search.addEventListener('input', () => view.setFilter(search.value));
  const tools = h('div', { class: 'toolbar' },
    segmented({ label: t('pages.container.lines'), size: 'sm', value: tail, options: [{ value: '100', label: '100' }, { value: '500', label: '500' }, { value: '5000', label: '5000' }, { value: 'all', label: t('pages.containers.all') }], onChange: (v) => { tail = v; connect(); } }),
    h('label', { class: 'row' }, toggle({ checked: false, labelledby: 'ts-label', onChange: (v) => { stamps = v; connect(); } }), h('span', { id: 'ts-label' }, t('pages.container.times'))),
    h('div', { class: 'fui-field__search grow' }, icon('search', 14), search),
    button({ size: 'sm', onClick: () => view.follow(true) }, icon('arrowDown', 13), t('pages.container.to_the_end')),
    button({ size: 'sm', href: base(`/containers/${encodeURIComponent(id)}/logs${qs({ tail: 'all', timestamps: 1 })}`), download: '' }, icon('download', 13), t('common.download')));
  connect();
  return h('div', { class: 'stack' }, tools, view.el);
}

// Two ways in: exec runs a new command beside the main process (a shell, usually); attach joins the main process itself,
// as `docker attach` does, to see what it prints and, when it reads its input, to type into it ("Streams" 3).
function consoleTab(c, running, cleanup) {
  const id = c.Id, cfg = c.Config || {};
  if (!running) return emptyState(t('pages.container.not_running'), t('pages.container.console_not_running'));
  if (!can('console')) return emptyState(t('pages.container.not_for_role'), t('pages.container.console_role'));
  const slot = h('div');
  let term = null, mode = 'exec';
  const cmd = formField({ id: 'exec-cmd', label: t('pages.container.command'), placeholder: '/bin/sh' });
  cmd.input.value = '/bin/sh';
  const user = formField({ id: 'exec-user', label: t('pages.container.as_user'), placeholder: t('pages.container.own_user') });
  const path = (cols, rows) => mode === 'attach'
    ? `/containers/${encodeURIComponent(id)}/attach/ws${qs({ cols, rows })}`
    : `/containers/${encodeURIComponent(id)}/exec/ws${qs({ cmd: cmd.input.value, user: user.input.value, cols, rows })}`;
  const open = () => {
    if (term) term.destroy();
    term = terminal({ connect: (cols, rows) => socket(base(path(cols, rows))) });
    mount(slot, term.el);
  };
  cleanup(() => term && term.destroy());
  const quick = (q) => button({ size: 'sm', variant: 'ghost', onClick: () => { cmd.input.value = q; open(); } }, q);
  const typing = cfg.OpenStdin && !cfg.StdinOnce;
  const attachNote = !typing
    ? (cfg.OpenStdin ? t('pages.container.attach_closes') : t('pages.container.attach_no_input'))
    : cfg.Tty
      ? t('pages.container.attach_tty')
      : t('pages.container.attach_input');
  const execTools = h('div', { class: 'stack' },
    h('div', { class: 'toolbar console-tools row--bottom' }, h('div', { class: 'grow' }, cmd.el), user.el, button({ variant: 'primary', onClick: open }, icon('terminal', 14), t('pages.networks.connect'))),
    h('div', { class: 'row' }, h('span', { class: 'faint' }, t('pages.container.or')), quick('/bin/bash'), quick('/bin/ash'), quick('/bin/sh')));
  const attachTools = h('div', { class: 'toolbar console-tools' },
    h('p', { class: 'muted grow' }, ...tParts('pages.container.attach_main', { cmd: h('span', { class: 'mono' }, [...(cfg.Entrypoint || []), ...(cfg.Cmd || [])].join(' ') || '?') }), ' ', attachNote),
    button({ variant: 'primary', onClick: open }, icon('terminal', 14), t('pages.container.attach')));
  const tools = h('div', {}, execTools);
  const pick = segmented({ label: t('pages.containers.console'), size: 'sm', value: mode, options: [{ value: 'exec', label: t('pages.container.run_command') }, { value: 'attach', label: t('pages.container.attach_main_process') }], onChange: (v) => {
    mode = v;
    if (term) { term.destroy(); term = null; }
    mount(slot);
    mount(tools, v === 'attach' ? attachTools : execTools);
  } });
  return h('div', { class: 'stack' }, pick, tools, slot);
}

const N = 60;
function statsTab(id, running, cleanup, memLimit) {
  if (!running) return emptyState(t('pages.container.not_running'), t('pages.container.stopped_uses_nothing'));
  const tiles = h('div', { class: 'fui-stat-tile__grid' });
  const charts = h('div', { class: 'grid-2' });
  let ws = null;
  // One run of samples at one interval; choosing another starts the charts again, so one line is never two speeds.
  function listen(every) {
    if (ws) ws.close();
    const cpu = [], mem = [], rx = [], tx = [], labels = [];
    let last = null, lastAt = 0;
    ws = socket(base(`/containers/${encodeURIComponent(id)}/stats/ws`) + qs({ every }));
    ws.onmessage = (e) => {
      let s; try { s = JSON.parse(e.data); } catch { return; }
      if (s.error) { mount(tiles, callout({ tone: 'critical', body: s.error })); return; }
      const now = new Date(), secs = lastAt ? Math.max(0.001, (now - lastAt) / 1000) : 1;
      const push = (a, v) => { a.push(v); if (a.length > N) a.shift(); };
      push(labels, clock(now, true));
      push(cpu, s.cpu); push(mem, s.mem);
      // Bytes since the last sample over the seconds between them: per second at any interval.
      push(rx, last ? Math.max(0, s.net_rx - last.net_rx) / secs : 0); push(tx, last ? Math.max(0, s.net_tx - last.net_tx) / secs : 0);
      last = s; lastAt = now;
      mount(tiles,
        statTile({ label: 'CPU', value: pct(s.cpu), hint: t('pages.container.cpu_hint', { count: s.cpus, all: pct(s.cpus * 100, 0) }) }),
        statTile({ label: t('pages.dashboard.memory'), value: bytes(s.mem), hint: memLimit ? t('pages.container.mem_of_limit', { limit: bytes(memLimit) }) : t('pages.container.mem_no_limit', { total: bytes(s.mem_limit) }) }),
        statTile({ label: t('pages.container.network'), value: `↓ ${bytes(rx[rx.length - 1])}/s`, hint: t('pages.container.net_hint', { up: bytes(tx[tx.length - 1]), rx: bytes(s.net_rx), tx: bytes(s.net_tx) }) }),
        statTile({ label: t('pages.dashboard.disk'), value: bytes(s.disk_read + s.disk_write), hint: t('pages.container.disk_hint', { read: bytes(s.disk_read), written: bytes(s.disk_write), count: s.pids }) }));
      mount(charts,
        card({ title: 'CPU', body: lineChart({ labels, series: [{ name: 'CPU %', values: cpu }], format: (v) => pct(v, v < 10 ? 1 : 0), height: 180 }) }),
        card({ title: t('pages.dashboard.memory'), body: lineChart({ labels, series: [{ name: t('pages.dashboard.memory'), values: mem }], format: bytes, height: 180, limit: memLimit ? { value: memLimit, label: t('pages.container.limit') } : null }) }),
        card({ title: t('pages.container.network'), body: lineChart({ labels, series: [{ name: t('pages.container.in'), values: rx }, { name: t('pages.container.out'), values: tx }], format: (v) => `${bytes(v)}/s`, height: 180 }) }));
    };
  }
  const every = segmented({ label: t('pages.container.every'), size: 'sm', value: String(statsEvery()),
    options: STATS_EVERY.map((n) => ({ value: String(n), label: t('units.seconds', { count: n }) })),
    onChange: (v) => { setStatsEvery(Number(v)); listen(statsEvery()); } });
  listen(statsEvery());
  cleanup(() => { if (ws) ws.close(); });
  return h('div', { class: 'stack' }, h('div', { class: 'toolbar' }, h('span', { class: 'muted' }, t('pages.container.sample_every')), every), tiles, charts);
}

const RANGES = [['1', 'hours', 1], ['6', 'hours', 6], ['24', 'hours', 24], ['168', 'days', 7], ['720', 'days', 30]].map(([value, unit, count]) => ({ value, label: t(`pages.container.range.${unit}`, { count }) }));

/** What it used over the hours before now, as Freightdock sampled it ("History" 1), stopped or not. */
function historyTab(id, signal, memLimit) {
  let hours = '24';
  const slot = h('div');
  const stamp = (at) => {
    const d = new Date(at * 1000);
    return Number(hours) > 24 ? `${day(d)} ${clock(d)}` : clock(d);
  };
  const draw = (r) => {
    const p = r.points || [];
    if (!p.length) return emptyState(t('pages.container.no_history'), r.enabled ? t('pages.container.history_soon', { count: r.every_s }) : t('pages.container.history_off'));
    const labels = p.map((x) => stamp(x.at));
    const rate = (v) => `${bytes(v)}/s`;
    return h('div', { class: 'grid-2' },
      card({ title: 'CPU', body: lineChart({ labels, series: [{ name: 'CPU %', values: p.map((x) => x.cpu) }], format: (v) => pct(v, v < 10 ? 1 : 0), height: 180 }) }),
      card({ title: t('pages.dashboard.memory'), body: lineChart({ labels, series: [{ name: t('pages.dashboard.memory'), values: p.map((x) => x.mem) }], format: bytes, height: 180, limit: memLimit ? { value: memLimit, label: t('pages.container.limit') } : null }) }),
      card({ title: t('pages.container.network'), body: lineChart({ labels, series: [{ name: t('pages.container.in'), values: p.map((x) => x.net_rx) }, { name: t('pages.container.out'), values: p.map((x) => x.net_tx) }], format: rate, height: 180 }) }),
      card({ title: t('pages.dashboard.disk'), body: lineChart({ labels, series: [{ name: t('pages.container.read'), values: p.map((x) => x.disk_read) }, { name: t('pages.container.written'), values: p.map((x) => x.disk_write) }], format: rate, height: 180 }) }));
  };
  const reload = loadInto(slot, { signal, load: (s) => get(base(`/containers/${encodeURIComponent(id)}/history${qs({ hours })}`), { signal: s }), draw });
  const tools = h('div', { class: 'toolbar' }, segmented({ label: t('pages.container.over'), size: 'sm', value: hours, options: RANGES, onChange: (v) => { hours = v; reload(); } }));
  return h('div', { class: 'stack' }, tools, slot);
}

function processesTab(id, running, signal) {
  if (!running) return emptyState(t('pages.container.not_running'), t('pages.container.no_processes_stopped'));
  const slot = h('div');
  loadInto(slot, { signal, load: (s) => get(base(`/containers/${encodeURIComponent(id)}/top`), { signal: s }), draw: (top) => table({
    rows: top.Processes || [], empty: [t('pages.container.no_processes'), ''],
    columns: (top.Titles || []).map((title, i) => ({ label: title, cls: /PID|%|TIME|C$/.test(title) ? 'mono' : null, cell: (r) => h('span', { class: i === top.Titles.length - 1 ? 'mono' : '' }, r[i]) })),
  }) });
  return slot;
}

function header(c, reload) {
  const name = (c.Name || '').replace(/^\//, ''), id = c.Id, st = c.State || {};
  const running = !!st.Running && !st.Paused, paused = !!st.Paused;
  const health = st.Health && st.Health.Status;
  const b = (label, ic, onClick, variant) => button({ size: 'sm', variant, onClick }, icon(ic, 13), label);
  const operate = can('operate'), manage = can('manage');
  const more = [
    operate && running ? { label: t('pages.containers.pause'), icon: 'pause', onSelect: () => act(id, name, 'pause', reload) } : null,
    operate && paused ? { label: t('pages.containers.resume'), icon: 'play', onSelect: () => act(id, name, 'unpause', reload) } : null,
    operate && running ? { label: t('pages.containers.kill'), icon: 'zap', onSelect: () => act(id, name, 'kill', reload) } : null,
    manage ? { label: t('pages.rename.rename'), icon: 'type', onSelect: () => renameContainer(id, name) } : null,
    can('create') ? { label: t('pages.containers.duplicate'), icon: 'copy', href: `/containers/new?from=${encodeURIComponent(name)}` } : null,
    manage ? { label: t('pages.containers.recreate'), icon: 'repeat', onSelect: () => recreate(id, name, reload) } : null,
    manage ? { separator: true } : null,
    manage ? { label: t('common.remove'), icon: 'trash', danger: true, onSelect: async () => { if (await removeContainer(id, name, running)) navigate('/containers'); } } : null,
  ].filter(Boolean);
  const actions = operate || manage ? h('div', { class: 'row' },
    operate && !running && !paused ? b(t('pages.containers.start'), 'play', () => act(id, name, 'start', reload), 'primary') : null,
    operate && running ? b(t('pages.containers.stop'), 'stop', () => act(id, name, 'stop', reload)) : null,
    operate && (running || paused) ? b(t('pages.containers.restart'), 'refresh', () => act(id, name, 'restart', reload)) : null,
    manage ? button({ size: 'sm', href: `/containers/${encodeURIComponent(name)}/edit` }, icon('pencil', 13), t('common.edit')) : null,
    more.length ? dropdownMenu({ label: t('common.more'), size: 'sm', items: more }) : null) : null;
  return pageHeader(h('span', { class: 'row' }, name, stateBadge(paused ? 'paused' : st.Status, health)), `${c.Config.Image} · ${running ? t('pages.container.up_since', { time: span(Date.now() / 1000 - Date.parse(st.StartedAt) / 1000) }) : tOr(`docker.state.${st.Status}`, st.Status)}`, actions);
}

export function containerPage({ el, params, query, signal, cleanup }) {
  const id = params.id;
  const slot = h('div');
  el.append(slot);
  let tab = query.get('tab') || 'overview';
  // The streams of what is drawn now (logs, stats, a console): they end when it is drawn again, or the page is left.
  let ends = [];
  const endAll = () => { ends.forEach((f) => { try { f(); } catch { /* gone already */ } }); ends = []; };
  cleanup(endAll);
  const scoped = (fn) => ends.push(fn);
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(base(`/containers/${encodeURIComponent(id)}`), { signal: s }),
    draw: (c) => {
      const st = c.State || {};
      const running = !!st.Running && !st.Paused;
      endAll();
      const own = (draw) => draw;
      const tabsEl = tabs({ label: t('pages.container.container'), value: tab, onChange: (k) => { tab = k; history.replaceState(null, '', k === 'overview' ? location.pathname : `?tab=${k}`); }, tabs: [
        { key: 'overview', label: t('pages.container.overview'), panel: own(() => overview(c, reload)) },
        { key: 'logs', label: t('pages.containers.logs'), panel: own(() => logsTab(c.Id, scoped)) },
        { key: 'console', label: t('pages.containers.console'), panel: own(() => consoleTab(c, running, scoped)) },
        { key: 'stats', label: t('pages.containers.stats'), panel: own(() => statsTab(c.Id, running, scoped, (c.HostConfig || {}).Memory || 0)) },
        { key: 'history', label: t('pages.container.history'), panel: own(() => historyTab(c.Id, signal, (c.HostConfig || {}).Memory || 0)) },
        { key: 'processes', label: t('pages.container.processes'), panel: own(() => processesTab(c.Id, running, signal)) },
        { key: 'inspect', label: t('pages.container.inspect'), panel: own(() => codeBlock({ panes: [{ key: 'json', label: 'JSON', text: JSON.stringify(c, null, 2) }], label: t('pages.container.dockers_inspect') })) },
      ] });
      return h('div', { class: 'stack' }, header(c, reload), tabsEl);
    },
  });
}

export { chip, chipSet };
