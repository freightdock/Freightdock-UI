// Integrations ("Cluster" 23): everything the cluster works with and how it behaves, in one place, for an
// administrator. The shared storage (moved here from the Cluster page), the fencing provider (Proxmox: its address,
// token and pinned certificate, read and tried from a manager; which guest each worker is), the cluster's timings
// (heartbeat, when a node is suspect or unreachable, the self-fencing timers, recovery), each saying where its value
// comes from, and what each manager's own environment sets, shown and not changed (a node needs it to reach the
// cluster at all). The cluster's settings are replicated: one change is every manager's.

import { h, mount } from '../dom.js';
import { get, post, put } from '../api.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { facts } from '../../finui/components/facts/facts.js';
import { callout } from '../../finui/components/callout/callout.js';
import { toast } from '../../finui/components/toast/toast.js';
import { confirm } from '../confirm.js';
import { t } from '../i18n.js';
import { nodeName, storageTable, storageDialog } from './cluster.js';

/** The timings, in the order shown: the setting, and the unit it is typed in (the API keeps ms or s as named). */
const TIMINGS = [
  ['heartbeat_ms', 'ms'], ['suspect_ms', 'ms'], ['unreachable_ms', 'ms'],
  ['fence_stop_s', 's'], ['fence_storage_s', 's'], ['fence_reboot_s', 's'], ['recovery_delay_s', 's'],
];
const SOURCE_TONES = { cluster: 'good', manager: 'warning', default: 'info' };

export function integrationsPage({ el, signal }) {
  const head = h('div'), body = h('div');
  el.append(head, body);
  mount(head, pageHeader(t('pages.integrations.title'), t('pages.integrations.sub')));
  const reload = loadInto(body, {
    signal,
    load: async (s) => {
      const [c, st, n] = await Promise.all([get('/api/cluster/settings', { signal: s }), get('/api/cluster/storage', { signal: s }).catch(() => null), get('/api/cluster/nodes', { signal: s }).catch(() => [])]);
      return { c, st, n };
    },
    draw: ({ c, st, n }) => {
      const s = c.settings || {};
      return h('div', { class: 'stack' },
        card({ title: t('pages.integrations.shares'), sub: t('pages.integrations.shares_sub'),
          actions: button({ size: 'sm', onClick: () => storageDialog(reload) }, t('pages.cluster.storage_add')),
          body: st ? storageTable(st, n || [], reload) : emptyState(t('pages.cluster.no_answer'), '') }),
        card({ title: t('pages.integrations.fencing'), sub: t('pages.integrations.fencing_sub'),
          actions: h('div', { class: 'row' },
            s.proxmox ? button({ size: 'sm', onClick: () => tryProxmox(null) }, t('pages.integrations.test')) : null,
            button({ size: 'sm', variant: s.proxmox ? null : 'primary', onClick: () => proxmoxDialog(s.proxmox, reload) }, s.proxmox ? t('pages.integrations.change') : t('pages.integrations.set_up')),
            s.proxmox && s.proxmox.source === 'cluster' ? button({ size: 'sm', variant: 'danger', onClick: () => removeProxmox(reload) }, t('common.remove')) : null),
          body: h('div', { class: 'stack' }, proxmoxFacts(s.proxmox), guestTable(n || [], !!s.proxmox, reload)) }),
        card({ title: t('pages.integrations.timings'), sub: t('pages.integrations.timings_sub'), body: timings(s, reload) }),
        card({ title: t('pages.integrations.manager'), sub: t('pages.integrations.manager_sub'), body: managerFacts(c.manager || {}) }));
    },
  });
}

function source(x) {
  return status({ tone: SOURCE_TONES[x] || 'info' }, t(`pages.integrations.source.${x || 'default'}`));
}

function proxmoxFacts(p) {
  if (!p) return callout({ tone: 'info', title: t('pages.integrations.no_provider'), body: t('pages.integrations.no_provider_text') });
  return facts([
    [t('pages.integrations.proxmox_url'), p.url, { mono: true }],
    [t('pages.integrations.proxmox_token'), p.token_id, { mono: true }],
    [t('pages.integrations.from'), source(p.source)],
    [t('pages.integrations.proxmox_fingerprint'), p.fingerprint, { wide: true, mono: true }],
  ]);
}

/** Proxmox's address, token and certificate: the fingerprint read from the server by a manager, to compare with the
 *  one Proxmox shows (its Certificates page) before it is pinned; tried before it is saved. */
function proxmoxDialog(p, reload) {
  const url = formField({ id: 'int-px-url', label: t('pages.integrations.proxmox_url'), placeholder: 'https://192.0.2.20:8006', help: t('pages.integrations.proxmox_url_help') });
  const fp = formField({ id: 'int-px-fp', label: t('pages.integrations.proxmox_fingerprint'), placeholder: 'AB:CD:...', help: t('pages.integrations.proxmox_fingerprint_help') });
  const token = formField({ id: 'int-px-token', type: 'password', autocomplete: 'off', label: t('pages.integrations.proxmox_token'),
    placeholder: p ? t('pages.integrations.proxmox_token_kept', { id: p.token_id }) : 'freightdock-fence@pve!fence=…', help: t('pages.integrations.proxmox_token_help') });
  if (p) { url.input.value = p.url; fp.input.value = p.fingerprint; }
  const read = button({ size: 'sm', onClick: async () => {
    read.disabled = true;
    try { fp.input.value = (await post('/api/cluster/settings/proxmox/fingerprint', { url: url.input.value.trim() })).fingerprint; fp.setError(''); } catch (e) { url.setError(e.message); }
    read.disabled = false;
  } }, t('pages.integrations.read_fingerprint'));
  const result = h('div');
  const body = () => {
    const b = { url: url.input.value.trim(), fingerprint: fp.input.value.trim() };
    if (token.input.value.trim()) b.token = token.input.value.trim();
    return b;
  };
  const test = button({ onClick: async () => { test.disabled = true; mount(result, await testResult(body())); test.disabled = false; } }, t('pages.integrations.test'));
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, url.el, h('div', { class: 'row' }, read), fp.el, token.el,
    h('p', { class: 'sub' }, t('pages.integrations.proxmox_rights')), result, h('div', { class: 'row row--end' }, test, go));
  const m = openModal({ title: t('pages.integrations.proxmox'), body: form, initialFocus: url.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    go.disabled = true;
    try {
      await put('/api/cluster/settings', { proxmox: body() });
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; token.setError(x.message); }
  });
}

/** What a try of the provider found: Proxmox's version and each worker's guest as Proxmox sees it. */
async function testResult(b) {
  try {
    const r = await post('/api/cluster/settings/proxmox/test', b || {});
    const guests = r.guests || [];
    return callout({ tone: guests.some((g) => g.error) ? 'warning' : 'good', title: t('pages.integrations.test_ok', { version: r.version }),
      body: guests.length ? h('ul', null, guests.map((g) => h('li', null, `${g.name}: `, g.guest ? h('span', { class: 'mono' }, g.guest) : null, ' ', g.error ? g.error : t('pages.integrations.guest_is', { status: g.status }))))
        : t('pages.integrations.test_no_guests') });
  } catch (e) {
    return callout({ tone: 'critical', title: t('pages.integrations.test_failed'), body: e.message });
  }
}

async function tryProxmox() {
  const out = h('div', null, t('pages.integrations.testing'));
  openModal({ title: t('pages.integrations.test'), body: out });
  mount(out, await testResult(null));
}

async function removeProxmox(reload) {
  if (!(await confirm({ title: t('pages.integrations.remove_title'), body: t('pages.integrations.remove_body') }))) return;
  try { await put('/api/cluster/settings', { proxmox: null }); toast({ text: t('pages.integrations.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** Each worker and how it is fenced: by itself, or powered off through Proxmox as the guest named by its labels. */
function guestTable(nodes, provider, reload) {
  const l = (n) => n.all_labels || n.labels || {};
  return table({
    rows: nodes, empty: [t('pages.integrations.no_workers'), ''], label: t('pages.integrations.guests'),
    columns: [
      { label: t('pages.integrations.worker'), cell: (n) => h('a', { href: `/cluster/nodes/${n.id}` }, nodeName(n)), sort: (n) => nodeName(n) },
      { label: t('pages.integrations.fenced_by'), cell: (n) => (l(n)['fence.provider'] === 'proxmox'
        ? status({ tone: provider ? 'good' : 'critical' }, provider ? t('pages.integrations.by_proxmox') : t('pages.integrations.by_proxmox_unset'))
        : status({ tone: 'info' }, t('pages.integrations.by_self'))), sort: (n) => l(n)['fence.provider'] || '' },
      { label: t('pages.integrations.guest'), cell: (n) => (l(n)['fence.provider'] === 'proxmox'
        ? h('span', { class: 'mono' }, `${l(n)['proxmox.node'] || '?'}/${l(n)['proxmox.type'] || 'lxc'}/${l(n)['proxmox.vmid'] || '?'}`) : h('span', { class: 'sub' }, '–')) },
      { label: '', nosort: true, cell: (n) => h('div', { class: 'row row--end' }, button({ size: 'sm', onClick: () => guestDialog(n, reload) }, t('pages.integrations.change'))) },
    ],
  });
}

const FENCE_KEYS = ['fence.provider', 'proxmox.node', 'proxmox.vmid', 'proxmox.type'];

/** How one worker is fenced: its labels, the others kept as they are. */
function guestDialog(n, reload) {
  const own = { ...(n.labels || {}) };
  let by = own['fence.provider'] === 'proxmox' ? 'proxmox' : 'self';
  let kind = own['proxmox.type'] || 'lxc';
  const pve = formField({ id: 'int-g-node', label: t('pages.integrations.pve_node'), placeholder: 'pve1', help: t('pages.integrations.pve_node_help') });
  const vmid = formField({ id: 'int-g-vmid', label: t('pages.integrations.vmid'), placeholder: '201', inputMode: 'numeric', help: t('pages.integrations.vmid_help') });
  pve.input.value = own['proxmox.node'] || '';
  vmid.input.value = own['proxmox.vmid'] || '';
  const kindSeg = segmented({ label: t('pages.integrations.guest_kind'), value: kind, size: 'sm',
    options: [{ value: 'lxc', label: t('pages.integrations.lxc') }, { value: 'qemu', label: t('pages.integrations.qemu') }], onChange: (v) => { kind = v; } });
  const px = h('div', { class: 'stack' }, pve.el, vmid.el, h('div', { class: 'fui-field' }, h('span', { class: 'fui-field__label' }, t('pages.integrations.guest_kind')), kindSeg));
  px.hidden = by !== 'proxmox';
  const bySeg = segmented({ label: t('pages.integrations.fenced_by'), value: by, size: 'sm',
    options: [{ value: 'self', label: t('pages.integrations.by_self') }, { value: 'proxmox', label: t('pages.integrations.by_proxmox') }],
    onChange: (v) => { by = v; px.hidden = v !== 'proxmox'; } });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true },
    h('div', { class: 'fui-field' }, h('span', { class: 'fui-field__label' }, t('pages.integrations.fenced_by')), bySeg), px,
    h('p', { class: 'sub' }, t('pages.integrations.by_help')), h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.integrations.guest_title', { name: nodeName(n) }), body: form });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const labels = Object.fromEntries(Object.entries(own).filter(([k]) => !FENCE_KEYS.includes(k)));
    if (by === 'proxmox') {
      if (!/^\d+$/.test(vmid.input.value.trim())) { vmid.setError(t('pages.integrations.vmid_help')); return; }
      Object.assign(labels, { 'fence.provider': 'proxmox', 'proxmox.node': pve.input.value.trim(), 'proxmox.vmid': vmid.input.value.trim(), 'proxmox.type': kind });
    }
    go.disabled = true;
    try {
      await put(`/api/cluster/nodes/${encodeURIComponent(n.id)}/labels`, { labels });
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; pve.setError(x.message); }
  });
}

/** The timings: numbers wait for Save, the two switches save at once. Each says where its value comes from; one the
 *  cluster set can go back to the managers' own. */
function timings(s, reload) {
  const save = async (patch, done) => {
    try { await put('/api/cluster/settings', patch); toast({ text: t('pages.integrations.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); if (done) done(); }
  };
  const back = (key) => (s[key] && s[key].source === 'cluster'
    ? button({ size: 'sm', variant: 'ghost', onClick: () => save({ [key]: null }) }, t('pages.integrations.use_default')) : null);
  const help = (key, extra) => h('span', null, t(`pages.integrations.help.${key}`), ' ', extra);
  const inputs = {};
  const rows = TIMINGS.map(([key, unit]) => {
    const v = s[key] || {};
    const input = h('input', { class: ['fui-field__input', 'fui-field__input--num'], id: `int-${key}`, type: 'number', min: v.min, max: v.max, value: v.value, inputMode: 'numeric' });
    inputs[key] = input;
    return settingRow({ id: `int-row-${key}`, label: t(`pages.integrations.label.${key}`), labelFor: `int-${key}`,
      help: help(key, t('pages.integrations.default_is', { value: v.default, unit })),
      control: h('div', { class: 'row nowrap' }, input, h('span', { class: 'sub' }, unit), source(v.source), back(key)) });
  });
  const switches = ['fence', 'auto_authorize'].map((key) => {
    const v = s[key] || {};
    return settingRow({ id: `int-row-${key}`, label: t(`pages.integrations.label.${key}`), help: help(key, ''),
      control: h('div', { class: 'row nowrap' }, source(v.source), back(key),
        toggle({ checked: v.value, labelledby: `int-row-${key}-label`, onChange: (on, set) => save({ [key]: on }, () => set(!on)) })) });
  });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true },
    h('div', { class: 'fui-setting-row__rows' }, switches.flat(), rows.flat()),
    h('p', { class: 'sub' }, t('pages.integrations.timings_note')),
    h('div', { class: 'fui-setting-row__actions' }, go));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const patch = {};
    for (const [key] of TIMINGS) {
      const n = Number(inputs[key].value);
      if (inputs[key].value !== '' && n !== (s[key] || {}).value) patch[key] = n;
    }
    if (Object.keys(patch).length) save(patch); else toast({ text: t('pages.integrations.nothing_changed'), tone: 'info' });
  });
  return form;
}

/** What only a manager's environment sets: shown as the manager answering has it, never changed here. */
function managerFacts(env) {
  const keys = Object.keys(env);
  if (!keys.length) return emptyState(t('pages.cluster.no_answer'), '');
  return h('div', { class: 'stack' }, facts(keys.map((k) => [k, env[k], { mono: true, wide: true }])), h('p', { class: 'sub' }, t('pages.integrations.manager_note')));
}
