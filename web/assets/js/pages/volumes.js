// Volumes: where containers keep their data, and who mounts each; for administrators, a way into each one's files.

import { h, ago, when } from '../dom.js';
import { get, post, del, qs } from '../api.js';
import { base } from '../docker.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { confirm } from '../confirm.js';
import { accessPicker, accessCard, describe } from '../ownership.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { openDrawer } from '../../finui/components/drawer/drawer.js';
import { formField, textArea } from '../../finui/components/field/field.js';
import { facts } from '../../finui/components/facts/facts.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

const toMap = (s) => Object.fromEntries(s.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => { const i = l.indexOf('='); return i < 0 ? [l, ''] : [l.slice(0, i), l.slice(i + 1)]; }));

function createDialog(after) {
  const name = formField({ id: 'vol-name', label: t('common.name'), placeholder: t('pages.volumes.name_placeholder'), autocomplete: 'off' });
  const driver = formField({ id: 'vol-driver', label: t('pages.volumes.driver'), placeholder: 'local' });
  const opts = textArea({ id: 'vol-opts', label: t('pages.volumes.driver_options'), rows: 3, help: t('pages.volumes.driver_options_help') });
  // Who may see it ("Access" 18): private to its maker unless said otherwise.
  const access = accessPicker({ id: 'vol-access' });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.create'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, driver.el, opts.el, access.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.volumes.create_title'), body: form, initialFocus: name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); name.setError('');
    try {
      const o = toMap(opts.input.value);
      await post(base('/volumes'), { Name: name.input.value.trim() || null, Driver: driver.input.value.trim() || 'local', DriverOpts: Object.keys(o).length ? o : null, access: access.value() });
      m.close(); toast({ text: t('common.created'), tone: 'good' }); after();
    } catch (err) { name.setError(err.message); }
  });
}

async function remove(v, after) {
  const used = (v.containers || []).length;
  const ok = await confirm({ title: t('common.remove_name', { name: v.Name }), body: used ? t('pages.volumes.remove_used', { count: used }) : t('pages.volumes.remove_body') });
  if (!ok) return;
  try { await del(base(`/volumes/${encodeURIComponent(v.Name)}`)); toast({ text: t('common.removed'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

function details(v, after) {
  let dr = null;
  const own = accessCard({ path: base(`/volumes/${encodeURIComponent(v.Name)}/access`), current: v.freightdock_access, reload: () => { if (dr) dr.close(); after(); } });
  dr = openDrawer({ title: v.Name, body: h('div', { class: 'stack' }, can('administer') ? h('div', { class: 'row' }, button({ size: 'sm', href: `/volumes/${encodeURIComponent(v.Name)}/files` }, icon('folderOpen', 13), t('pages.volumes.browse_files'))) : null, facts([
    [t('pages.volumes.driver'), v.Driver], [t('pages.volumes.scope'), v.Scope], [t('common.made'), v.CreatedAt ? when(Date.parse(v.CreatedAt) / 1000) : null],
    [t('pages.volumes.mount_point'), v.Mountpoint, { wide: true, mono: true }],
    [t('pages.volumes.used_by'), (v.containers || []).join(', ') || t('pages.volumes.nothing'), { wide: true }],
    [t('pages.volumes.options'), Object.entries(v.Options || {}).map(([k, x]) => `${k}=${x}`).join(', '), { wide: true, mono: true }],
    [t('common.labels'), Object.entries(v.Labels || {}).map(([k, x]) => `${k}=${x}`).join('\n'), { wide: true, mono: true }],
  ]), own) });
}

export function volumesPage({ el, signal }) {
  let words = [];
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(base('/volumes'), { signal: s }),
    draw: (list, again) => table({
      rows: list.filter((v) => matches(words, v.Name, v.Driver, ...(v.containers || []))), filter: false, empty: [t('pages.volumes.empty_title'), t('pages.volumes.empty_text')],
      columns: [
        { label: t('common.name'), cell: (v) => h('div', { class: 'name-cell' }, h('button', { type: 'button', class: 'link-button mono truncate', onClick: () => details(v, again) }, v.Name),
          v.Labels && v.Labels['com.docker.compose.project'] ? h('span', { class: 'sub' }, t('common.stack_name', { name: v.Labels['com.docker.compose.project'] })) : null) },
        { label: t('pages.volumes.used_by'), cell: (v) => ((v.containers || []).length ? h('span', { class: 'row' }, (v.containers || []).slice(0, 3).map((c) => h('a', { class: 'fui-chip', href: `/containers/${encodeURIComponent(c)}` }, c)), v.containers.length > 3 ? h('span', { class: 'faint' }, `+${v.containers.length - 3}`) : null) : status({ tone: 'warning', line: true }, t('pages.volumes.unused'))), sort: (v) => (v.containers || []).length },
        { label: t('pages.volumes.driver'), cell: (v) => v.Driver },
        { label: t('ownership.who_may_see'), cell: (v) => describe(v.freightdock_access), sort: (v) => describe(v.freightdock_access) },
        { label: t('common.made'), cell: (v) => (v.CreatedAt ? h('span', { class: 'nowrap' }, ago(Date.parse(v.CreatedAt) / 1000)) : ''), sort: (v) => Date.parse(v.CreatedAt || 0) },
        { label: '', nosort: true, cell: (v) => (can('administer') ? h('div', { class: 'cell-actions' },
          button({ variant: 'icon', size: 'sm', href: `/volumes/${encodeURIComponent(v.Name)}/files`, 'aria-label': t('pages.volumes.browse_files_in', { name: v.Name }) }, icon('folderOpen', 14)),
          button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: v.Name }), onClick: () => remove(v, again) }, icon('trash', 14))) : '') },
      ],
    }),
  });
  const actions = h('div', { class: 'row' },
    can('administer') ? button({ size: 'sm', onClick: async () => {
      const ok = await confirm({ title: t('pages.volumes.prune_title'), body: t('pages.volumes.prune_body'), option: { label: t('pages.volumes.prune_all'), help: t('pages.volumes.prune_all_help') } });
      if (!ok) return;
      try { const r = await post(base('/volumes/prune'), { all: ok.option }); toast({ text: t('common.n_removed', { count: (r.VolumesDeleted || []).length }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
    } }, icon('trash', 13), t('common.clean_up')) : null,
    can('create') ? button({ size: 'sm', variant: 'primary', onClick: () => createDialog(reload) }, icon('plus', 14), t('pages.volumes.create')) : null);
  el.append(pageHeader(t('nav.volumes'), t('pages.volumes.sub'), actions), h('div', { class: 'toolbar' }, filterBox(t('pages.volumes.filter'), (w) => { words = w; reload.redraw(); })), slot);
}

export { qs };
