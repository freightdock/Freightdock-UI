// Networks: how containers reach each other, which are on each, and joining or leaving one.

import { h, ago } from '../dom.js';
import { get, post, del } from '../api.js';
import { base } from '../docker.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { confirm } from '../confirm.js';
import { accessPicker, accessCard, describe } from '../ownership.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { openDrawer } from '../../finui/components/drawer/drawer.js';
import { formField } from '../../finui/components/field/field.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { facts } from '../../finui/components/facts/facts.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t, lang } from '../i18n.js';

const subnet = (n) => ((n.IPAM && n.IPAM.Config) || []).map((c) => c.Subnet).filter(Boolean).join(', ');

function createDialog(after) {
  const name = formField({ id: 'net-name', label: t('common.name'), autocomplete: 'off' });
  const sub = formField({ id: 'net-subnet', label: t('pages.networks.subnet'), placeholder: t('pages.networks.subnet_placeholder') });
  const gw = formField({ id: 'net-gw', label: t('pages.networks.gateway'), placeholder: t('pages.networks.gateway_placeholder') });
  let driver = 'bridge', internal = false, attachable = true, ipv6 = false;
  const flag = (id, label, help, value, set) => settingRow({ id, label, help, control: toggle({ checked: value, labelledby: `${id}-label`, onChange: set }) });
  // Who may see it ("Access" 18): private to its maker unless said otherwise.
  const access = accessPicker({ id: 'net-access' });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.create'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el,
    settingRow({ id: 'net-driver', label: t('pages.volumes.driver'), control: segmented({ label: t('pages.volumes.driver'), size: 'sm', value: driver, options: ['bridge', 'macvlan', 'ipvlan', 'overlay'].map((d) => ({ value: d, label: d })), onChange: (v) => { driver = v; } }) }),
    sub.el, gw.el,
    flag('net-internal', t('pages.networks.internal'), t('pages.networks.internal_help'), internal, (v) => { internal = v; }),
    flag('net-attachable', t('pages.networks.attachable'), t('pages.networks.attachable_help'), attachable, (v) => { attachable = v; }),
    flag('net-ipv6', 'IPv6', t('pages.networks.ipv6_help'), ipv6, (v) => { ipv6 = v; }),
    access.el,
    h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.networks.create_title'), body: form, initialFocus: name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); name.setError(''); sub.setError('');
    const cfg = sub.input.value.trim() ? [{ Subnet: sub.input.value.trim(), Gateway: gw.input.value.trim() || undefined }] : null;
    try {
      await post(base('/networks'), { Name: name.input.value.trim(), Driver: driver, Internal: internal, Attachable: attachable, EnableIPv6: ipv6, IPAM: cfg ? { Driver: 'default', Config: cfg } : null, access: access.value() });
      m.close(); toast({ text: t('common.created'), tone: 'good' }); after();
    } catch (err) { (err.field === 'name' ? name : sub).setError(err.message); }
  });
}

async function details(n, after) {
  const d = await get(base(`/networks/${encodeURIComponent(n.Id)}`));
  const members = Object.entries(d.Containers || {});
  const box = h('div', { class: 'stack' });
  const draw = () => box.replaceChildren(
    facts([[t('pages.volumes.driver'), d.Driver], [t('pages.volumes.scope'), d.Scope], [t('pages.networks.subnet'), subnet(d), { mono: true }], [t('pages.networks.internal'), d.Internal ? t('common.yes') : t('common.no')], [t('pages.networks.attachable'), d.Attachable ? t('common.yes') : t('common.no')], ['Id', d.Id.slice(0, 12), { mono: true }]]),
    h('h3', { class: 'drawer-sub' }, t('pages.networks.on_it')),
    members.length ? h('ul', { class: 'plain-list' }, members.map(([id, c]) => h('li', { class: 'row' },
      h('a', { href: `/containers/${encodeURIComponent(c.Name)}`, class: 'grow' }, c.Name), h('span', { class: 'mono faint' }, (c.IPv4Address || '').split('/')[0]),
      can('manage') && !n.built_in ? button({ size: 'sm', variant: 'ghost', onClick: async () => {
        if (!(await confirm({ title: t('pages.networks.disconnect_title', { container: c.Name, network: d.Name }), body: t('pages.networks.disconnect_body'), action: t('pages.networks.disconnect') }))) return;
        try { await post(base(`/networks/${encodeURIComponent(d.Id)}/disconnect`), { container: id, force: false }); toast({ text: t('pages.networks.disconnected'), tone: 'good' }); dr.close(); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
      } }, t('pages.networks.disconnect')) : null))) : h('p', { class: 'muted' }, t('common.none')),
    can('manage') && !['host', 'none'].includes(d.Name) ? connectForm(d, () => { dr.close(); after(); }) : null,
    n.built_in ? null : accessCard({ path: base(`/networks/${encodeURIComponent(d.Id)}/access`), current: d.freightdock_access, reload: () => { dr.close(); after(); } }));
  draw();
  const dr = openDrawer({ title: d.Name, body: box });
}

function connectForm(d, after) {
  const c = formField({ id: 'net-join', label: t('pages.networks.connect_a_container'), placeholder: t('pages.networks.its_name') });
  const alias = formField({ id: 'net-alias', label: t('pages.networks.aliases'), placeholder: t('pages.networks.aliases_placeholder') });
  const go = button({ size: 'sm', type: 'submit' }, t('pages.networks.connect'));
  const form = h('form', { class: 'stack', novalidate: true }, c.el, alias.el, h('div', { class: 'row row--end' }, go));
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); c.setError('');
    try { await post(base(`/networks/${encodeURIComponent(d.Id)}/connect`), { container: c.input.value.trim(), aliases: alias.input.value.split(',').map((x) => x.trim()).filter(Boolean) }); toast({ text: t('pages.networks.connected'), tone: 'good' }); after(); } catch (err) { c.setError(err.message); }
  });
  return form;
}

export function networksPage({ el, signal }) {
  let words = [];
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(base('/networks'), { signal: s }),
    draw: (list, again) => table({
      rows: list.filter((n) => matches(words, n.Name, n.Driver, subnet(n))).sort((a, b) => (a.built_in - b.built_in) || a.Name.localeCompare(b.Name, lang())), filter: false, empty: [t('pages.networks.empty'), ''],
      columns: [
        { label: t('common.name'), cell: (n) => h('div', { class: 'name-cell' }, h('button', { type: 'button', class: 'link-button', onClick: () => details(n, again) }, n.Name), n.built_in ? h('span', { class: 'sub' }, t('pages.networks.dockers_own')) : null) },
        { label: t('pages.volumes.driver'), cell: (n) => n.Driver },
        { label: t('ownership.who_may_see'), cell: (n) => describe(n.freightdock_access), sort: (n) => describe(n.freightdock_access) },
        { label: t('pages.networks.subnet'), cell: (n) => h('span', { class: 'mono' }, subnet(n)) },
        { label: t('nav.containers'), cls: 'r', cell: (n) => (n.container_count ? String(n.container_count) : h('span', { class: 'faint' }, '0')), sort: (n) => n.container_count },
        { label: t('pages.networks.kind'), cell: (n) => h('span', { class: 'row' }, n.Internal ? status({ tone: 'info', line: true }, t('pages.networks.internal_badge')) : null, n.Attachable ? status({ tone: 'info', line: true }, t('pages.networks.attachable_badge')) : null) },
        { label: t('common.made'), cell: (n) => h('span', { class: 'nowrap' }, ago(Date.parse(n.Created) / 1000)), sort: (n) => Date.parse(n.Created) },
        { label: '', nosort: true, cell: (n) => (can('administer') && !n.built_in ? h('div', { class: 'cell-actions' }, button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: n.Name }), onClick: async () => {
          if (!(await confirm({ title: t('common.remove_name', { name: n.Name }), body: n.container_count ? t('pages.networks.remove_used') : t('pages.networks.remove_unused') }))) return;
          try { await del(base(`/networks/${encodeURIComponent(n.Id)}`)); toast({ text: t('common.removed'), tone: 'good' }); again(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
        } }, icon('trash', 14))) : '') },
      ],
    }),
  });
  const actions = h('div', { class: 'row' },
    can('administer') ? button({ size: 'sm', onClick: async () => {
      if (!(await confirm({ title: t('pages.networks.prune_title'), body: t('pages.networks.prune_body') }))) return;
      try { const r = await post(base('/networks/prune')); toast({ text: t('common.n_removed', { count: (r.NetworksDeleted || []).length }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
    } }, icon('trash', 13), t('common.clean_up')) : null,
    can('create') ? button({ size: 'sm', variant: 'primary', onClick: () => createDialog(reload) }, icon('plus', 14), t('pages.networks.create')) : null);
  el.append(pageHeader(t('nav.networks'), t('pages.networks.sub'), actions), h('div', { class: 'toolbar' }, filterBox(t('pages.networks.filter'), (w) => { words = w; reload.redraw(); })), slot);
}
