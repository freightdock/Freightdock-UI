// Integrations ("Cluster" 23): everything the cluster works with and how it behaves, in one place, for an
// administrator. The shared storage (moved here from the Cluster page), the fencing providers (Proxmox: its address,
// token and pinned certificate, read and tried from a manager; Redfish: one account for the nodes' BMCs ("Cluster"
// 29); which guest or BMC each worker is), the cluster's timings
// (heartbeat, when a node is suspect or unreachable, the self-fencing timers, recovery), each saying where its value
// comes from, and what each manager's own environment sets, shown and not changed (a node needs it to reach the
// cluster at all). The cluster's settings are replicated: one change is every manager's. Backups of the cluster: one
// now as a download, or each manager's scheduled ones with the passphrase they are sealed with (set, never shown).
// Private registries: a login per registry the workers pull with ("Cluster" 26), the password never shown.
// Notifications: channels the leading manager tells the cluster's news through ("Cluster" 27), each secret never shown.
// Import from Portainer ("Portainer" 11): a Portainer's Compose stacks, read only, brought in as cluster stacks.

import { h, mount, bytes, when, ago } from '../dom.js';
import { get, post, put, del, download } from '../api.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField, textArea } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { facts } from '../../finui/components/facts/facts.js';
import { callout } from '../../finui/components/callout/callout.js';
import { checkbox, choiceGroup } from '../../finui/components/choice/choice.js';
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
      const [c, st, n, b, rg, nt, pt] = await Promise.all([get('/api/cluster/settings', { signal: s }), get('/api/cluster/storage', { signal: s }).catch(() => null), get('/api/cluster/nodes', { signal: s }).catch(() => []),
        get('/api/cluster/backups', { signal: s }).catch(() => null), get('/api/cluster/registries', { signal: s }).catch(() => null),
        get('/api/cluster/notifications', { signal: s }).catch(() => null), get('/api/portainer', { signal: s }).catch(() => null)]);
      return { c, st, n, b, rg, nt, pt };
    },
    draw: ({ c, st, n, b, rg, nt, pt }) => {
      const s = c.settings || {};
      return h('div', { class: 'stack' },
        card({ title: t('pages.integrations.shares'), sub: t('pages.integrations.shares_sub'),
          actions: button({ size: 'sm', onClick: () => storageDialog(reload) }, t('pages.cluster.storage_add')),
          body: st ? storageTable(st, n || [], reload) : emptyState(t('pages.cluster.no_answer'), '') }),
        card({ title: t('pages.integrations.registries'), sub: t('pages.integrations.registries_sub'),
          actions: button({ size: 'sm', onClick: () => registryDialog(null, reload) }, t('pages.integrations.registry_add')),
          body: rg ? registryTable(rg, reload) : emptyState(t('pages.cluster.no_answer'), '') }),
        card({ title: t('pages.integrations.notifications'), sub: t('pages.integrations.notifications_sub'),
          actions: nt ? button({ size: 'sm', onClick: () => channelDialog(null, nt.events, reload) }, t('pages.integrations.channel_add')) : null,
          body: nt ? channelTable(nt, reload) : emptyState(t('pages.cluster.no_answer'), '') }),
        card({ title: t('pages.integrations.fencing'), sub: t('pages.integrations.fencing_sub'),
          body: h('div', { class: 'stack' },
            s.proxmox || s.redfish ? null : callout({ tone: 'info', title: t('pages.integrations.no_provider'), body: t('pages.integrations.no_provider_text') }),
            proxmoxBlock(s.proxmox, reload), redfishBlock(s.redfish, reload), guestTable(n || [], { proxmox: !!s.proxmox, redfish: !!s.redfish }, reload)) }),
        card({ title: t('pages.integrations.backups'), sub: t('pages.integrations.backups_sub'),
          actions: button({ size: 'sm', variant: 'primary', onClick: () => backupNow() }, t('pages.integrations.backup_now')),
          body: backups(s, reload) }),
        card({ title: t('pages.integrations.stored'), sub: t('pages.integrations.stored_sub'), body: storedBackups(b, !!s.backup_passphrase_set) }),
        pt ? portainerCard(pt, reload) : null,
        card({ title: t('pages.integrations.timings'), sub: t('pages.integrations.timings_sub'), body: timings(s, reload) }),
        card({ title: t('pages.integrations.manager'), sub: t('pages.integrations.manager_sub'), body: managerFacts(c.manager || {}) }));
    },
  });
}

/** Import from Portainer: the connection (its token never shown), and a look at its stacks to bring in. */
function portainerCard(p, reload) {
  const actions = h('div', { class: 'row' },
    p.connected ? button({ size: 'sm', variant: 'primary', onClick: () => portainerScan() }, t('pages.integrations.portainer_look')) : null,
    button({ size: 'sm', onClick: () => portainerDialog(p, reload) }, p.connected ? t('pages.integrations.change') : t('pages.integrations.portainer_connect')),
    p.connected ? button({ size: 'sm', variant: 'danger', onClick: () => forgetPortainer(reload) }, t('pages.integrations.portainer_forget')) : null);
  const body = p.connected
    ? facts([[t('pages.integrations.portainer_url'), p.url, { mono: true }], [t('pages.integrations.portainer_version'), p.version], [t('pages.integrations.portainer_trust'), p.trust]])
    : h('p', { class: 'sub' }, t('pages.integrations.portainer_none'));
  return card({ title: t('pages.integrations.portainer'), sub: t('pages.integrations.portainer_sub'), actions, body });
}

function portainerDialog(p, reload) {
  const url = formField({ id: 'int-pt-url', label: t('pages.integrations.portainer_url'), placeholder: 'https://portainer.example:9443' });
  const token = formField({ id: 'int-pt-token', type: 'password', autocomplete: 'off', label: t('pages.integrations.portainer_token'),
    placeholder: p.connected ? t('pages.integrations.portainer_token_kept') : 'ptr_…', help: t('pages.integrations.portainer_token_help') });
  const ca = textArea({ id: 'int-pt-ca', label: t('pages.integrations.portainer_ca'), help: t('pages.integrations.portainer_ca_help'), placeholder: '-----BEGIN CERTIFICATE-----', rows: 4 });
  let skip = !!p.skip_verify;
  const skipBox = checkbox({ label: t('pages.integrations.portainer_skip'), help: t('pages.integrations.portainer_skip_help'), checked: skip, onChange: (v) => { skip = v; } });
  if (p.connected) { url.input.value = p.url; ca.input.value = p.ca || ''; }
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.integrations.portainer_connect'));
  const form = h('form', { class: 'stack', novalidate: true }, url.el, token.el, ca.el, skipBox, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.integrations.portainer'), body: form, initialFocus: url.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    url.setError(''); token.setError('');
    go.disabled = true;
    try {
      const b = { url: url.input.value.trim(), ca: ca.input.value, skip_verify: skip };
      if (token.input.value.trim()) b.token = token.input.value.trim();
      await put('/api/portainer', b);
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; (x.field === 'token' ? token : url).setError(x.message); }
  });
}

async function forgetPortainer(reload) {
  if (!(await confirm({ title: t('pages.integrations.portainer_forget_title'), body: t('pages.integrations.portainer_forget_body') }))) return;
  try { await del('/api/portainer'); toast({ text: t('pages.integrations.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** Portainer's stacks, each ticked when it can come, and the import: made stopped unless they are to start now. */
async function portainerScan() {
  const out = h('div', null, t('pages.integrations.portainer_reading'));
  const m = openModal({ title: t('pages.integrations.portainer_stacks'), wide: true, body: out });
  let r;
  try { r = await post('/api/portainer/scan', {}); } catch (e) { mount(out, callout({ tone: 'critical', title: t('pages.integrations.portainer_failed'), body: e.message })); return; }
  const picked = new Set(r.stacks.filter((s) => !s.why).map((s) => s.id));
  let start = false;
  const rows = table({
    rows: r.stacks, empty: [t('pages.integrations.portainer_no_stacks'), ''], label: t('pages.integrations.portainer_stacks'),
    columns: [
      { label: '', nosort: true, cell: (s) => checkbox({ label: s.name, checked: picked.has(s.id), disabled: !!s.why, onChange: (v) => { if (v) picked.add(s.id); else picked.delete(s.id); } }) },
      { label: t('pages.integrations.portainer_env'), cell: (s) => s.env, sort: (s) => s.env },
      { label: t('pages.integrations.portainer_as'), cell: (s) => (s.why ? h('span', { class: 'sub' }, s.why) : h('span', null, h('span', { class: 'mono' }, s.target),
        s.git ? h('span', { class: 'sub' }, ' ' + t('pages.integrations.portainer_git')) : null,
        s.active ? null : h('span', { class: 'sub' }, ' ' + t('pages.integrations.portainer_inactive')))) },
    ],
  });
  const startBox = checkbox({ label: t('pages.integrations.portainer_start'), help: t('pages.integrations.portainer_start_help'), onChange: (v) => { start = v; } });
  const result = h('div');
  const go = button({ variant: 'primary', onClick: async () => {
    if (!picked.size) return;
    go.disabled = true;
    try {
      const done = await post('/api/portainer/import', { stacks: [...picked], start });
      const ok = done.results.filter((x) => x.ok);
      mount(result, callout({ tone: ok.length === done.results.length ? 'good' : 'warning', title: t('pages.integrations.portainer_done', { count: ok.length }),
        body: h('ul', null, done.results.map((x) => h('li', null, `${x.name || x.id}: `, x.ok ? h('span', { class: 'mono' }, x.stack) : x.error,
          x.ok && x.left_out && x.left_out.length ? ' ' + t('pages.integrations.portainer_left_out', { names: x.left_out.join(', ') }) : null))) }));
    } catch (e) { mount(result, callout({ tone: 'critical', title: t('pages.integrations.portainer_failed'), body: e.message })); }
    go.disabled = false;
  } }, t('pages.integrations.portainer_import'));
  mount(out, h('div', { class: 'stack' }, h('p', { class: 'sub' }, t('pages.integrations.portainer_from', { url: r.url, version: r.version })), rows, startBox, result, h('div', { class: 'row row--end' }, go)));
  void m;
}

function source(x) {
  return status({ tone: SOURCE_TONES[x] || 'info' }, t(`pages.integrations.source.${x || 'default'}`));
}

/** Proxmox: its address, token id and pinned certificate, with what can be done about it. */
function proxmoxBlock(p, reload) {
  const actions = h('div', { class: 'row' },
    p ? button({ size: 'sm', onClick: () => tryProxmox() }, t('pages.integrations.test')) : null,
    button({ size: 'sm', onClick: () => proxmoxDialog(p, reload) }, p ? t('pages.integrations.change') : t('pages.integrations.set_up')),
    p && p.source === 'cluster' ? button({ size: 'sm', variant: 'danger', onClick: () => removeProxmox(reload) }, t('common.remove')) : null);
  return h('div', { class: 'stack' },
    h('div', { class: 'row' }, h('strong', null, t('pages.integrations.proxmox')), actions),
    p ? proxmoxFacts(p) : h('p', { class: 'sub' }, t('pages.integrations.proxmox_none')));
}

function proxmoxFacts(p) {
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

/** The Redfish account the managers log in to the nodes' BMCs with: its user, never its password. */
function redfishBlock(r, reload) {
  const actions = h('div', { class: 'row' },
    r ? button({ size: 'sm', onClick: () => tryRedfish() }, t('pages.integrations.test')) : null,
    button({ size: 'sm', onClick: () => redfishDialog(r, reload) }, r ? t('pages.integrations.change') : t('pages.integrations.redfish_set_up')),
    r && r.source === 'cluster' ? button({ size: 'sm', variant: 'danger', onClick: () => removeRedfish(reload) }, t('common.remove')) : null);
  return h('div', { class: 'stack' },
    h('div', { class: 'row' }, h('strong', null, t('pages.integrations.redfish')), actions),
    r ? facts([[t('pages.integrations.redfish_user'), r.username, { mono: true }], [t('pages.integrations.from'), source(r.source)]])
      : h('p', { class: 'sub' }, t('pages.integrations.redfish_none')));
}

function redfishDialog(r, reload) {
  const user = formField({ id: 'int-rf-user', label: t('pages.integrations.redfish_user'), placeholder: 'freightdock-fence', autocomplete: 'off' });
  const pw = formField({ id: 'int-rf-pw', type: 'password', autocomplete: 'new-password', label: t('pages.integrations.redfish_password'),
    placeholder: r ? t('pages.integrations.redfish_password_kept') : '', help: t('pages.integrations.redfish_password_help') });
  if (r) user.input.value = r.username;
  const body = () => {
    const b = { username: user.input.value.trim() };
    if (pw.input.value) b.password = pw.input.value;
    return b;
  };
  const result = h('div');
  const test = button({ onClick: async () => { test.disabled = true; mount(result, await redfishResult(body())); test.disabled = false; } }, t('pages.integrations.test'));
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, user.el, pw.el,
    h('p', { class: 'sub' }, t('pages.integrations.redfish_rights')), result, h('div', { class: 'row row--end' }, test, go));
  const m = openModal({ title: t('pages.integrations.redfish'), body: form, initialFocus: user.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    go.disabled = true;
    try {
      await put('/api/cluster/settings', { redfish: body() });
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; pw.setError(x.message); }
  });
}

/** What a try of the account found: each redfish worker's BMC, its system and its power state. */
async function redfishResult(b) {
  try {
    const r = await post('/api/cluster/settings/redfish/test', b || {});
    const sys = r.systems || [];
    return callout({ tone: sys.some((x) => x.error) ? 'warning' : 'good', title: t('pages.integrations.redfish_tried', { n: sys.length }),
      body: sys.length ? h('ul', null, sys.map((x) => h('li', null, `${x.name}: `, x.bmc ? h('span', { class: 'mono' }, x.bmc) : null, ' ',
        x.error ? x.error : t('pages.integrations.power_is', { power: x.power }))))
        : t('pages.integrations.redfish_no_nodes') });
  } catch (e) {
    return callout({ tone: 'critical', title: t('pages.integrations.test_failed_redfish'), body: e.message });
  }
}

async function tryRedfish() {
  const out = h('div', null, t('pages.integrations.testing_redfish'));
  openModal({ title: t('pages.integrations.test'), body: out });
  mount(out, await redfishResult(null));
}

async function removeRedfish(reload) {
  if (!(await confirm({ title: t('pages.integrations.redfish_remove_title'), body: t('pages.integrations.redfish_remove_body') }))) return;
  try { await put('/api/cluster/settings', { redfish: null }); toast({ text: t('pages.integrations.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

const BY = { proxmox: ['by_proxmox', 'by_proxmox_unset'], redfish: ['by_redfish', 'by_redfish_unset'] };

/** Each worker and how it is fenced: by itself, powered off through Proxmox as the guest named by its labels, or
 *  through its BMC. */
function guestTable(nodes, set, reload) {
  const l = (n) => n.all_labels || n.labels || {};
  const by = (n) => {
    const p = l(n)['fence.provider'];
    if (!BY[p]) return status({ tone: 'info' }, t('pages.integrations.by_self'));
    return status({ tone: set[p] ? 'good' : 'critical' }, t(`pages.integrations.${BY[p][set[p] ? 0 : 1]}`));
  };
  const what = (n) => {
    const x = l(n);
    if (x['fence.provider'] === 'proxmox') return h('span', { class: 'mono' }, `${x['proxmox.node'] || '?'}/${x['proxmox.type'] || 'lxc'}/${x['proxmox.vmid'] || '?'}`);
    if (x['fence.provider'] === 'redfish') return h('span', { class: 'mono' }, x['redfish.system'] ? `${x['redfish.host'] || '?'} ${x['redfish.system']}` : (x['redfish.host'] || '?'));
    return h('span', { class: 'sub' }, '–');
  };
  return table({
    rows: nodes, empty: [t('pages.integrations.no_workers'), ''], label: t('pages.integrations.guests'),
    columns: [
      { label: t('pages.integrations.worker'), cell: (n) => h('a', { href: `/cluster/nodes/${n.id}` }, nodeName(n)), sort: (n) => nodeName(n) },
      { label: t('pages.integrations.fenced_by'), cell: by, sort: (n) => l(n)['fence.provider'] || '' },
      { label: t('pages.integrations.guest'), cell: what },
      { label: '', nosort: true, cell: (n) => h('div', { class: 'row row--end' }, button({ size: 'sm', onClick: () => guestDialog(n, reload) }, t('pages.integrations.change'))) },
    ],
  });
}

const FENCE_KEYS = ['fence.provider', 'proxmox.node', 'proxmox.vmid', 'proxmox.type', 'redfish.host', 'redfish.fingerprint', 'redfish.system'];

/** How one worker is fenced: its labels, the others kept as they are. */
function guestDialog(n, reload) {
  const own = { ...(n.labels || {}) };
  let by = BY[own['fence.provider']] ? own['fence.provider'] : 'self';
  let kind = own['proxmox.type'] || 'lxc';
  const pve = formField({ id: 'int-g-node', label: t('pages.integrations.pve_node'), placeholder: 'pve1', help: t('pages.integrations.pve_node_help') });
  const vmid = formField({ id: 'int-g-vmid', label: t('pages.integrations.vmid'), placeholder: '201', inputMode: 'numeric', help: t('pages.integrations.vmid_help') });
  pve.input.value = own['proxmox.node'] || '';
  vmid.input.value = own['proxmox.vmid'] || '';
  const kindSeg = segmented({ label: t('pages.integrations.guest_kind'), value: kind, size: 'sm',
    options: [{ value: 'lxc', label: t('pages.integrations.lxc') }, { value: 'qemu', label: t('pages.integrations.qemu') }], onChange: (v) => { kind = v; } });
  const px = h('div', { class: 'stack' }, pve.el, vmid.el, h('div', { class: 'fui-field' }, h('span', { class: 'fui-field__label' }, t('pages.integrations.guest_kind')), kindSeg));
  px.hidden = by !== 'proxmox';
  const bmc = formField({ id: 'int-g-bmc', label: t('pages.integrations.bmc_host'), placeholder: '192.0.2.31', help: t('pages.integrations.bmc_host_help') });
  const bfp = formField({ id: 'int-g-bfp', label: t('pages.integrations.proxmox_fingerprint'), placeholder: 'AB:CD:...', help: t('pages.integrations.bmc_fingerprint_help') });
  const sys = formField({ id: 'int-g-sys', label: t('pages.integrations.bmc_system'), placeholder: 'System.Embedded.1', help: t('pages.integrations.bmc_system_help') });
  bmc.input.value = own['redfish.host'] || '';
  bfp.input.value = own['redfish.fingerprint'] || '';
  sys.input.value = own['redfish.system'] || '';
  const read = button({ size: 'sm', onClick: async () => {
    read.disabled = true;
    try { bfp.input.value = (await post('/api/cluster/settings/redfish/fingerprint', { host: bmc.input.value.trim() })).fingerprint; bfp.setError(''); } catch (e) { bmc.setError(e.message); }
    read.disabled = false;
  } }, t('pages.integrations.read_fingerprint'));
  const rf = h('div', { class: 'stack' }, bmc.el, h('div', { class: 'row' }, read), bfp.el, sys.el);
  rf.hidden = by !== 'redfish';
  const bySeg = segmented({ label: t('pages.integrations.fenced_by'), value: by, size: 'sm',
    options: [{ value: 'self', label: t('pages.integrations.by_self') }, { value: 'proxmox', label: t('pages.integrations.by_proxmox') }, { value: 'redfish', label: t('pages.integrations.by_redfish') }],
    onChange: (v) => { by = v; px.hidden = v !== 'proxmox'; rf.hidden = v !== 'redfish'; } });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true },
    h('div', { class: 'fui-field' }, h('span', { class: 'fui-field__label' }, t('pages.integrations.fenced_by')), bySeg), px, rf,
    h('p', { class: 'sub' }, t('pages.integrations.by_help')), h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.integrations.guest_title', { name: nodeName(n) }), body: form });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const labels = Object.fromEntries(Object.entries(own).filter(([k]) => !FENCE_KEYS.includes(k)));
    if (by === 'proxmox') {
      if (!/^\d+$/.test(vmid.input.value.trim())) { vmid.setError(t('pages.integrations.vmid_help')); return; }
      Object.assign(labels, { 'fence.provider': 'proxmox', 'proxmox.node': pve.input.value.trim(), 'proxmox.vmid': vmid.input.value.trim(), 'proxmox.type': kind });
    } else if (by === 'redfish') {
      if (!bmc.input.value.trim()) { bmc.setError(t('pages.integrations.bmc_host_help')); return; }
      if (!bfp.input.value.trim()) { bfp.setError(t('pages.integrations.bmc_fingerprint_help')); return; }
      Object.assign(labels, { 'fence.provider': 'redfish', 'redfish.host': bmc.input.value.trim(), 'redfish.fingerprint': bfp.input.value.trim() });
      if (sys.input.value.trim()) labels['redfish.system'] = sys.input.value.trim();
    }
    go.disabled = true;
    try {
      await put(`/api/cluster/nodes/${encodeURIComponent(n.id)}/labels`, { labels });
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; (by === 'redfish' ? bmc : pve).setError(x.message); }
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

function registryTable(rows, reload) {
  return table({
    rows, filter: false, label: t('pages.integrations.registries'), empty: [t('pages.integrations.no_registries'), t('pages.integrations.no_registries_text')],
    columns: [
      { label: t('pages.integrations.registry'), cell: (x) => h('span', { class: 'mono' }, x.host), sort: (x) => x.host },
      { label: t('pages.integrations.registry_user'), cell: (x) => h('span', { class: 'mono' }, x.username), sort: (x) => x.username },
      { label: '', nosort: true, cell: (x) => h('div', { class: 'row row--end nowrap' },
        button({ size: 'sm', onClick: () => tryRegistry(x.host, {}) }, t('pages.integrations.test')),
        button({ size: 'sm', onClick: () => registryDialog(x, reload) }, t('pages.integrations.change')),
        button({ size: 'sm', variant: 'danger', onClick: () => removeRegistry(x.host, reload) }, t('common.remove'))) },
    ],
  });
}

/** What a sign-in from a manager said. */
async function registryResult(host, body) {
  try {
    const r = await post(`/api/cluster/registries/${encodeURIComponent(host)}/test`, body);
    return callout({ tone: 'good', title: t('pages.integrations.registry_ok'), body: r.message || '' });
  } catch (e) {
    return callout({ tone: 'critical', title: t('pages.integrations.registry_failed'), body: e.message });
  }
}

async function tryRegistry(host, body) {
  const out = h('div', null, t('pages.integrations.registry_testing'));
  openModal({ title: t('pages.integrations.registry_test', { host }), body: out });
  mount(out, await registryResult(host, body));
}

/** A registry's login: its name as images have it, the user and a password or token (kept when left empty on a
 *  change), tried from a manager before it is saved if wanted. */
function registryDialog(x, reload) {
  const host = formField({ id: 'int-rg-host', label: t('pages.integrations.registry'), placeholder: 'ghcr.io', autocomplete: 'off', help: t('pages.integrations.registry_help') });
  const user = formField({ id: 'int-rg-user', label: t('pages.integrations.registry_user'), autocomplete: 'off' });
  const pass = formField({ id: 'int-rg-pass', type: 'password', autocomplete: 'new-password', label: t('pages.integrations.registry_password'),
    placeholder: x ? t('pages.integrations.registry_kept') : '', help: t('pages.integrations.registry_password_help') });
  if (x) { host.input.value = x.host; host.input.readOnly = true; user.input.value = x.username; }
  const result = h('div');
  const name = () => host.input.value.trim().toLowerCase();
  const body = () => {
    const b = { username: user.input.value.trim() };
    if (pass.input.value) b.password = pass.input.value;
    return b;
  };
  const test = button({ onClick: async () => {
    if (!name()) { host.setError(t('pages.integrations.registry_help')); return; }
    test.disabled = true; mount(result, await registryResult(name(), body())); test.disabled = false;
  } }, t('pages.integrations.test'));
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, host.el, user.el, pass.el, result, h('div', { class: 'row row--end' }, test, go));
  const m = openModal({ title: x ? t('pages.integrations.registry_change', { host: x.host }) : t('pages.integrations.registry_add'), body: form, initialFocus: x ? user.input : host.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (!name()) { host.setError(t('pages.integrations.registry_help')); return; }
    go.disabled = true;
    try {
      await put(`/api/cluster/registries/${encodeURIComponent(name())}`, body());
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (err) { go.disabled = false; host.setError(err.message); }
  });
}

async function removeRegistry(host, reload) {
  if (!(await confirm({ title: t('pages.integrations.registry_remove', { host }), body: t('pages.integrations.registry_remove_body') }))) return;
  try { await del(`/api/cluster/registries/${encodeURIComponent(host)}`); toast({ text: t('pages.integrations.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

const KIND_NAMES = ['webhook', 'email', 'ntfy', 'gotify'];
const evLabel = (e) => t(`pages.integrations.ev.${e.replace('.', '_')}`);

function channelLast(x) {
  if (!x.enabled) return status({ tone: 'info' }, t('pages.integrations.channel_off'));
  if (x.last_error) return h('span', { title: x.last_error }, status({ tone: 'critical' }, t('pages.integrations.channel_failed')), ' ', h('span', { class: 'muted' }, x.last_error));
  if (x.last_sent_at) return status({ tone: 'good' }, t('pages.integrations.channel_sent', { when: ago(x.last_sent_at / 1000) }));
  return h('span', { class: 'muted' }, t('pages.integrations.channel_never'));
}

function channelTable(nt, reload) {
  const all = (nt.events || []).length;
  return table({
    rows: nt.channels || [], filter: false, label: t('pages.integrations.notifications'), empty: [t('pages.integrations.no_channels'), t('pages.integrations.no_channels_text')],
    columns: [
      { label: t('pages.integrations.channel_name'), cell: (x) => h('span', { class: 'mono' }, x.name), sort: (x) => x.name },
      { label: t('pages.integrations.channel_kind'), cell: (x) => t(`pages.integrations.kind_${x.kind}`), sort: (x) => x.kind },
      { label: t('pages.integrations.channel_events'), cell: (x) => h('span', { title: (x.events || []).map(evLabel).join('\n') }, t('pages.integrations.channel_events_count', { n: (x.events || []).length, all })), sort: (x) => (x.events || []).length },
      { label: t('pages.integrations.channel_last'), cell: channelLast, sort: (x) => x.last_sent_at || 0 },
      { label: '', nosort: true, cell: (x) => h('div', { class: 'row row--end nowrap' },
        button({ size: 'sm', onClick: () => tryChannel(x.name, reload) }, t('pages.integrations.test')),
        button({ size: 'sm', onClick: () => channelDialog(x, nt.events, reload) }, t('pages.integrations.change')),
        button({ size: 'sm', variant: 'danger', onClick: () => removeChannel(x.name, reload) }, t('common.remove'))) },
    ],
  });
}

/** Sends a test through a channel as it is saved, from a manager, and says how it went. */
async function tryChannel(name, reload) {
  const out = h('div', null, t('pages.integrations.channel_testing'));
  openModal({ title: t('pages.integrations.channel_test', { name }), body: out });
  try {
    const r = await post(`/api/cluster/notifications/${encodeURIComponent(name)}/test`, {});
    mount(out, callout({ tone: 'good', title: t('pages.integrations.channel_ok'), body: r.message || '' }));
  } catch (e) {
    mount(out, callout({ tone: 'critical', title: t('pages.integrations.channel_failed_test'), body: e.message }));
  }
  reload();
}

/** A channel, set whole: its name, kind, the fields its kind reads, its secret (kept when left empty on a change, or
 *  taken away), the events it tells and whether it is on. The manager checks it and names the field that is wrong. */
function channelDialog(x, events, reload) {
  const c = (x && x.config) || {};
  let kind = (x && x.kind) || 'webhook';
  let chosen = x ? [...(x.events || [])] : (events || []).map((e) => e.event);
  let enabled = x ? !!x.enabled : true;
  let clear = false;
  const f = (id, label, opts = {}) => formField({ id: `int-ch-${id}`, label, autocomplete: 'off', ...opts });
  const name = f('name', t('pages.integrations.channel_name'), { help: t('pages.integrations.channel_name_help') });
  const url = f('url', t('pages.integrations.channel_url'), { placeholder: 'https://hooks.example/cluster', help: t('pages.integrations.channel_url_help') });
  const server = f('server', t('pages.integrations.channel_server'), { help: t('pages.integrations.channel_ntfy_server_help') });
  const topic = f('topic', t('pages.integrations.channel_topic'));
  const priority = f('priority', t('pages.integrations.channel_priority'), { type: 'number', help: t('pages.integrations.channel_ntfy_priority_help') });
  const host = f('host', t('pages.integrations.channel_smtp_host'), { placeholder: 'smtp.example' });
  const port = f('port', t('pages.integrations.channel_smtp_port'), { type: 'number', help: t('pages.integrations.channel_smtp_port_help') });
  const user = f('user', t('pages.integrations.channel_smtp_user'));
  const from = f('from', t('pages.integrations.channel_from'), { placeholder: 'cluster@example.org' });
  const to = f('to', t('pages.integrations.channel_to'), { help: t('pages.integrations.channel_to_help') });
  let security = c.security || 'starttls';
  const sec = h('div', { class: 'stack stack--tight' }, h('span', { class: 'fui-field__label' }, t('pages.integrations.channel_security')),
    segmented({ label: t('pages.integrations.channel_security'), value: security, onChange: (v) => { security = v; },
      options: [{ value: 'starttls', label: 'STARTTLS' }, { value: 'tls', label: 'TLS' }, { value: 'none', label: '-' }] }));
  const secret = f('secret', '', { type: 'password', autocomplete: 'new-password', placeholder: x && x.has_secret ? t('pages.integrations.secret_kept') : '', help: t('pages.integrations.secret_webhook_help') });
  const clearBox = x && x.has_secret ? checkbox({ label: t('pages.integrations.secret_clear'), onChange: (v) => { clear = v; } }) : null;
  if (x) { name.input.value = x.name; name.input.readOnly = true; }
  url.input.value = c.url || ''; server.input.value = c.server || ''; topic.input.value = c.topic || '';
  priority.input.value = c.priority ?? ''; host.input.value = c.host || ''; port.input.value = c.port ?? '';
  user.input.value = c.username || ''; from.input.value = c.from || ''; to.input.value = (c.to || []).join(', ');

  const fieldsOf = {
    webhook: [url.el], ntfy: [server.el, topic.el, priority.el], gotify: [server.el, priority.el],
    email: [host.el, port.el, sec, user.el, from.el, to.el],
  };
  const fields = h('div', { class: 'stack' });
  const secretLabel = secret.el.querySelector('label');
  const paint = () => {
    mount(fields, ...fieldsOf[kind]);
    if (secretLabel) secretLabel.textContent = t(`pages.integrations.secret_${kind}`);
    server.input.placeholder = kind === 'ntfy' ? 'https://ntfy.sh' : 'https://gotify.example';
    priority.input.placeholder = kind === 'ntfy' ? '4' : '5';
    const help = (field, text) => { const p = field.el.querySelector('.fui-field__help'); if (p) { p.textContent = text; p.hidden = !text; } };
    help(server, kind === 'ntfy' ? t('pages.integrations.channel_ntfy_server_help') : '');
    help(priority, kind === 'ntfy' ? t('pages.integrations.channel_ntfy_priority_help') : t('pages.integrations.channel_gotify_priority_help'));
    help(secret, t(`pages.integrations.secret_${kind}_help`));
  };
  const kinds = segmented({ label: t('pages.integrations.channel_kind'), value: kind, onChange: (v) => { kind = v; paint(); },
    options: KIND_NAMES.map((k) => ({ value: k, label: t(`pages.integrations.kind_${k}`) })) });
  const evs = choiceGroup({ label: t('pages.integrations.channel_events'), kind: 'checkbox', value: chosen, onChange: (v) => { chosen = v; },
    options: (events || []).map((e) => ({ value: e.event, label: evLabel(e.event), help: e.says })) });
  const on = checkbox({ label: t('pages.integrations.channel_enabled'), checked: enabled, onChange: (v) => { enabled = v; } });
  paint();

  const num = (i) => (i.input.value.trim() === '' ? null : Number(i.input.value));
  const body = () => {
    const config = {
      webhook: { url: url.input.value.trim() },
      ntfy: { server: server.input.value.trim(), topic: topic.input.value.trim(), priority: num(priority) },
      gotify: { server: server.input.value.trim(), priority: num(priority) },
      email: { host: host.input.value.trim(), port: num(port), security, username: user.input.value.trim(), from: from.input.value.trim(),
        to: to.input.value.split(',').map((v) => v.trim()).filter(Boolean) },
    }[kind];
    const b = { kind, enabled, events: chosen, config };
    if (clear) b.secret = ''; else if (secret.input.value) b.secret = secret.input.value;
    return b;
  };
  const error = h('div');
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el,
    h('div', { class: 'stack stack--tight' }, h('span', { class: 'fui-field__label' }, t('pages.integrations.channel_kind')), kinds),
    fields, secret.el, clearBox, evs, on, error, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: x ? t('pages.integrations.channel_change', { name: x.name }) : t('pages.integrations.channel_add'), body: form, initialFocus: x ? null : name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const n = name.input.value.trim();
    if (!/^[a-z0-9_-]{1,64}$/.test(n)) { name.setError(t('pages.integrations.channel_name_help')); return; }
    go.disabled = true; mount(error);
    try {
      await put(`/api/cluster/notifications/${encodeURIComponent(n)}`, body());
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (err) { go.disabled = false; mount(error, callout({ tone: 'critical', title: t('pages.integrations.channel_not_saved'), body: err.message })); }
  });
}

async function removeChannel(name, reload) {
  if (!(await confirm({ title: t('pages.integrations.channel_remove', { name }), body: t('pages.integrations.channel_remove_body') }))) return;
  try { await del(`/api/cluster/notifications/${encodeURIComponent(name)}`); toast({ text: t('pages.integrations.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** A passphrase field typed twice; `value()` is the passphrase, or null with the field marked when they differ or
 *  it is too short. */
function passphraseFields(prefix) {
  const one = formField({ id: `${prefix}-1`, type: 'password', autocomplete: 'new-password', label: t('pages.integrations.passphrase'), help: t('pages.integrations.passphrase_help') });
  const two = formField({ id: `${prefix}-2`, type: 'password', autocomplete: 'new-password', label: t('pages.integrations.passphrase_again') });
  const value = () => {
    const p = one.input.value;
    if ([...p].length < 12) { one.setError(t('pages.integrations.passphrase_short')); return null; }
    if (p !== two.input.value) { two.setError(t('pages.integrations.passphrase_differs')); return null; }
    one.setError(''); two.setError('');
    return p;
  };
  return { el: h('div', { class: 'stack' }, one.el, two.el), first: one.input, value, error: (m) => one.setError(m) };
}

/** A backup now: sealed by a manager with the passphrase typed here, downloaded, kept nowhere. */
function backupNow() {
  const p = passphraseFields('int-bk-now');
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.integrations.download'));
  const form = h('form', { class: 'stack', novalidate: true }, p.el, callout({ tone: 'warning', title: t('pages.integrations.backup_holds'), body: t('pages.integrations.backup_holds_text') }),
    h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.integrations.backup_now'), body: form, initialFocus: p.first });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const pass = p.value();
    if (!pass) return;
    go.disabled = true;
    try {
      await download('/api/cluster/backup', { passphrase: pass }, 'freightdock.fdbk');
      m.close();
      toast({ text: t('pages.integrations.backup_done'), tone: 'good' });
    } catch (x) { go.disabled = false; p.error(x.message); }
  });
}

/** The scheduled backups: the passphrase (set or not), how often and how many; how to bring one back. */
function backups(s, reload) {
  const set = !!s.backup_passphrase_set;
  const every = s.backup_every_h || {}, keep = s.backup_keep || {};
  const save = async (patch) => {
    try { await put('/api/cluster/settings', patch); toast({ text: t('pages.integrations.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
  };
  const num = (key, v, unit) => {
    const input = h('input', { class: ['fui-field__input', 'fui-field__input--num'], id: `int-${key}`, type: 'number', min: v.min, max: v.max, value: v.value, inputMode: 'numeric' });
    const row = settingRow({ id: `int-row-${key}`, label: t(`pages.integrations.label.${key}`), labelFor: `int-${key}`,
      help: h('span', null, t(`pages.integrations.help.${key}`), ' ', unit ? t('pages.integrations.default_is', { value: v.default, unit }) : t('pages.integrations.default_plain', { value: v.default })),
      control: h('div', { class: 'row nowrap' }, input, unit ? h('span', { class: 'sub' }, unit) : null, source(v.source)) });
    return { key, input, row, v };
  };
  const fields = [num('backup_every_h', every, 'h'), num('backup_keep', keep, '')];
  const passRow = settingRow({ id: 'int-row-backup_passphrase', label: t('pages.integrations.label.backup_passphrase'), help: t('pages.integrations.help.backup_passphrase'),
    control: h('div', { class: 'row nowrap' },
      status({ tone: set ? 'good' : 'warning' }, set ? t('pages.integrations.passphrase_set') : t('pages.integrations.passphrase_unset')),
      button({ size: 'sm', onClick: () => passphraseDialog(set, reload) }, set ? t('pages.integrations.change') : t('pages.integrations.passphrase_set_up')),
      set ? button({ size: 'sm', variant: 'ghost', onClick: async () => {
        if (await confirm({ title: t('pages.integrations.passphrase_off_title'), body: t('pages.integrations.passphrase_off_body') })) save({ backup_passphrase: null });
      } }, t('common.remove')) : null) });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, h('div', { class: 'fui-setting-row__rows' }, passRow, fields.map((f) => f.row)), h('div', { class: 'fui-setting-row__actions' }, go));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    const patch = {};
    for (const f of fields) {
      const n = Number(f.input.value);
      if (f.input.value !== '' && n !== f.v.value) patch[f.key] = n;
    }
    if (Object.keys(patch).length) save(patch); else toast({ text: t('pages.integrations.nothing_changed'), tone: 'info' });
  });
  return h('div', { class: 'stack' }, form,
    callout({ tone: 'info', title: t('pages.integrations.restore'), body: h('span', null, t('pages.integrations.restore_text'), ' ', h('code', null, t('pages.integrations.restore_command'))) }));
}

/** The scheduled backups on the manager that answered, newest first, each to download. */
function storedBackups(b, set) {
  return b ? table({
    rows: b.backups || [], empty: [t('pages.integrations.no_backups'), set ? '' : t('pages.integrations.no_backups_text')], label: t('pages.integrations.stored'),
    columns: [
      { label: t('pages.integrations.taken'), cell: (x) => when(x.taken_at / 1000), sort: (x) => x.taken_at },
      { label: t('pages.integrations.size'), cell: (x) => bytes(x.size), sort: (x) => x.size },
      { label: t('pages.integrations.on_manager'), cell: (x) => h('span', { class: 'mono' }, String(x.manager || '').slice(0, 8)) },
      { label: '', nosort: true, cell: (x) => h('div', { class: 'row row--end' }, button({ size: 'sm', href: `/api/cluster/backups/${encodeURIComponent(x.name)}`, download: x.name }, t('pages.integrations.download'))) },
    ],
  }) : emptyState(t('pages.cluster.no_answer'), '');
}

function passphraseDialog(set, reload) {
  const p = passphraseFields('int-bk-pass');
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, p.el, h('p', { class: 'sub' }, t('pages.integrations.passphrase_keep')), h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: set ? t('pages.integrations.passphrase_change') : t('pages.integrations.label.backup_passphrase'), body: form, initialFocus: p.first });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const pass = p.value();
    if (!pass) return;
    go.disabled = true;
    try {
      await put('/api/cluster/settings', { backup_passphrase: pass });
      m.close();
      toast({ text: t('pages.integrations.saved'), tone: 'good' });
      reload();
    } catch (x) { go.disabled = false; p.error(x.message); }
  });
}

/** What only a manager's environment sets: shown as the manager answering has it, never changed here. */
function managerFacts(env) {
  const keys = Object.keys(env);
  if (!keys.length) return emptyState(t('pages.cluster.no_answer'), '');
  return h('div', { class: 'stack' }, facts(keys.map((k) => [k, env[k], { mono: true, wide: true }])), h('p', { class: 'sub' }, t('pages.integrations.manager_note')));
}
