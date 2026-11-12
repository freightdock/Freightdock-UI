// A volume's files, for administrators: walk its folders, download a file (or a folder as a tar), upload files, make a
// folder, delete. Each request runs in a short-lived helper container on the volume's host ("Volume files").

import { h, bytes, ago, when } from '../dom.js';
import { get, post, del, qs, csrfToken } from '../api.js';
import { base } from '../docker.js';
import { loadInto, table } from '../view.js';
import { can } from '../state.js';
import { navigate } from '../router.js';
import { confirm } from '../confirm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { breadcrumb } from '../../finui/components/breadcrumb/breadcrumb.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { fileDrop } from '../../finui/components/file-drop/file-drop.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { callout } from '../../finui/components/callout/callout.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

const MAX = 256 * 1024 * 1024;
const join = (dir, name) => (dir ? `${dir}/${name}` : name);
const KIND_ICON = { dir: 'folder', file: 'file', link: 'link', other: 'file' };

/** Sends one file to `url` with the CSRF token, telling `progress(0..1)` as it goes. */
function send(url, file, progress) {
  return new Promise((resolve, reject) => {
    const x = new XMLHttpRequest();
    x.open('POST', url);
    x.setRequestHeader('X-CSRF-Token', csrfToken());
    x.setRequestHeader('Content-Type', 'application/octet-stream');
    x.upload.addEventListener('progress', (e) => { if (e.lengthComputable) progress(e.loaded / e.total); });
    x.addEventListener('load', () => {
      if (x.status >= 200 && x.status < 300) return resolve();
      let m = t('api.answered', { status: String(x.status) });
      try { m = JSON.parse(x.responseText).error || m; } catch { /* not JSON */ }
      reject(new Error(m));
    });
    x.addEventListener('error', () => reject(new Error(t('pages.backups.unreachable'))));
    x.send(file);
  });
}

function uploadDialog(volume, dir, after) {
  const rows = [];
  const drop = fileDrop({ label: t('pages.volume_files.drop'), help: t('pages.volume_files.drop_help', { dir: `/${dir}` }), multiple: true, maxSize: MAX, onFiles: async (taken, refused) => {
    for (const r of refused) rows.push({ name: r.file.name, size: r.file.size, error: r.reason });
    const mine = taken.map((f) => { const r = { name: f.name, size: f.size, progress: 0 }; rows.push(r); return [f, r]; });
    drop.show(rows);
    for (const [f, r] of mine) {
      try {
        await send(base(`/volumes/${encodeURIComponent(volume)}/files/upload${qs({ path: dir, name: f.name })}`), f, (p) => { r.progress = p; drop.show(rows); });
        r.progress = 1;
      } catch (e) { r.error = e.message; }
      drop.show(rows);
    }
    after();
  } });
  openModal({ title: t('pages.volume_files.upload_to', { dir: `/${dir}` }), body: drop, wide: true });
}

function folderDialog(volume, dir, after) {
  const name = formField({ id: 'folder-name', label: t('pages.volume_files.folder_name'), placeholder: 'config', autocomplete: 'off', help: t('pages.volume_files.folder_help', { dir: `/${dir}` }) });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.volume_files.make'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.volume_files.new_folder'), body: form, initialFocus: name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); name.setError('');
    try { await post(base(`/volumes/${encodeURIComponent(volume)}/files/mkdir`), { path: join(dir, name.input.value.trim()) }); m.close(); after(); }
    catch (x) { name.setError(x.message); }
  });
}

async function remove(volume, dir, item, after) {
  const body = item.kind === 'dir' ? t('pages.volume_files.delete_folder', { name: item.name, volume }) : t('pages.volume_files.delete_file', { name: item.name, volume });
  if (!(await confirm({ title: t('pages.volume_files.delete_title', { name: item.name }), body, action: t('common.delete') }))) return;
  try { await del(base(`/volumes/${encodeURIComponent(volume)}/files${qs({ path: join(dir, item.name) })}`)); toast({ text: t('pages.volume_files.deleted', { name: item.name }), tone: 'good' }); after(); }
  catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

export function volumeFilesPage({ el, params, query, signal }) {
  const volume = params.name;
  const dir = (query.get('path') || '').replace(/^\/+|\/+$/g, '');
  const go = (path) => navigate(`/volumes/${encodeURIComponent(volume)}/files${qs({ path })}`);
  if (!can('administer')) { el.append(pageHeader(volume, t('pages.stacks.files')), emptyState(t('pages.volume_files.admins_only'), t('pages.volume_files.admins_only_text'))); return; }
  const parts = dir ? dir.split('/') : [];
  const crumbs = breadcrumb({ items: [{ label: t('nav.volumes'), href: '/volumes' }, { label: volume, href: `/volumes/${encodeURIComponent(volume)}/files` },
    ...parts.map((p, i) => ({ label: p, href: `/volumes/${encodeURIComponent(volume)}/files${qs({ path: parts.slice(0, i + 1).join('/') })}` }))] });
  const slot = h('div');
  const href = (item) => base(`/volumes/${encodeURIComponent(volume)}/files/download${qs({ path: join(dir, item.name) })}`);
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(base(`/volumes/${encodeURIComponent(volume)}/files${qs({ path: dir })}`), { signal: s }),
    draw: (r, again) => h('div', { class: 'stack' },
      r.truncated ? callout({ tone: 'warning', body: t('pages.volume_files.truncated') }) : null,
      table({
        rows: r.entries, filter: 'auto', label: t('pages.stacks.files'), empty: [t('pages.volume_files.empty'), dir ? t('pages.volume_files.empty_folder') : t('pages.volume_files.empty_volume')],
        columns: [
          { label: t('common.name'), cell: (i) => h('span', { class: 'row' }, icon(KIND_ICON[i.kind] || 'file', 14),
            i.kind === 'dir' ? h('a', { href: `/volumes/${encodeURIComponent(volume)}/files${qs({ path: join(dir, i.name) })}` }, i.name) : h('span', { class: 'mono' }, i.name)), sort: (i) => `${i.kind === 'dir' ? 0 : 1}${i.name.toLowerCase()}` },
          { label: t('pages.container.size'), cls: 'r', cell: (i) => (i.kind === 'dir' ? '' : h('span', { class: 'mono' }, bytes(i.size))), sort: (i) => (i.kind === 'dir' ? -1 : i.size) },
          { label: t('pages.stack.changed'), cell: (i) => h('span', { class: 'nowrap', title: when(i.modified) }, ago(i.modified)), sort: (i) => i.modified },
          { label: t('pages.swarm_services.mode'), cell: (i) => h('span', { class: 'mono faint' }, i.mode) },
          { label: '', nosort: true, cell: (i) => h('div', { class: 'cell-actions' },
            i.kind === 'dir' || i.kind === 'file' ? button({ variant: 'icon', size: 'sm', href: href(i), download: '', 'aria-label': t(i.kind === 'dir' ? 'pages.volume_files.download_tar' : 'pages.backups.download', { name: i.name }) }, icon('download', 14)) : null,
            button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.volume_files.delete_label', { name: i.name }), onClick: () => remove(volume, dir, i, again) }, icon('trash', 14))) },
        ],
      })),
  });
  const actions = h('div', { class: 'row' },
    dir ? button({ size: 'sm', onClick: () => go(parts.slice(0, -1).join('/')) }, icon('arrowUp', 13), t('pages.volume_files.up')) : null,
    button({ size: 'sm', onClick: () => folderDialog(volume, dir, reload) }, icon('folderPlus', 13), t('pages.volume_files.new_folder')),
    button({ size: 'sm', variant: 'primary', onClick: () => uploadDialog(volume, dir, reload) }, icon('upload', 13), t('pages.volume_files.upload')));
  el.append(crumbs, pageHeader(dir ? `/${dir}` : volume, dir ? t('pages.volume_files.in_volume', { volume }) : t('pages.volume_files.volume_files'), actions), slot);
}
