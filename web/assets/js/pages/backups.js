// Backups, for administrators: where Freightdock's own data goes (a folder or S3), how often, how many are kept, the
// passphrase that seals every archive; the archives there, each to download or restore; and a restore from an archive
// brought from elsewhere. A restore has Freightdock start again, which puts it in place.

import { h, bytes, ago } from '../dom.js';
import { get, put, post, csrfToken } from '../api.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { button } from '../../finui/components/button/button.js';
import { formField } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { fileDrop } from '../../finui/components/file-drop/file-drop.js';
import { callout } from '../../finui/components/callout/callout.js';
import { spinner } from '../../finui/components/spinner/spinner.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

const field = (id, label, value, o = {}) => { const f = formField({ id, label, type: o.type || 'text', placeholder: o.placeholder, help: o.help, autocomplete: o.autocomplete || 'off' }); f.input.value = value == null ? '' : String(value); return f; };

function settingsCard(cfg, after) {
  let enabled = !!cfg.enabled, dest = cfg.destination || 'folder', virtualHost = !!(cfg.s3 && cfg.s3.virtual_host);
  const s3 = cfg.s3 || {};
  const f = {
    every_hours: field('every_hours', t('pages.backups.every_hours'), cfg.every_hours, { type: 'number', help: t('pages.backups.every_hours_help') }),
    keep: field('keep', t('pages.backups.keep'), cfg.keep, { type: 'number', help: t('pages.backups.keep_help') }),
    passphrase: field('passphrase', t('pages.backups.passphrase'), '', { type: 'password', autocomplete: 'new-password', placeholder: cfg.has_passphrase ? t('pages.registries.kept') : t('pages.backups.at_least_12'),
      help: t('pages.backups.passphrase_help') }),
    folder: field('folder', t('pages.backups.folder'), cfg.folder, { placeholder: cfg.default_folder || '<data>/backups', help: t('pages.backups.folder_help') }),
    endpoint: field('endpoint', t('pages.backups.endpoint'), s3.endpoint, { placeholder: t('pages.backups.endpoint_placeholder') }),
    region: field('region', t('pages.registries.region'), s3.region, { placeholder: 'us-east-1' }),
    bucket: field('bucket', t('pages.backups.bucket'), s3.bucket, { placeholder: 'backups', help: t('pages.backups.bucket_help') }),
    prefix: field('prefix', t('pages.backups.prefix'), s3.prefix, { placeholder: 'freightdock/' }),
    access_key: field('access_key', t('pages.backups.access_key'), s3.access_key),
    secret_key: field('secret_key', t('pages.registries.secret_ecr'), '', { type: 'password', placeholder: s3.has_secret ? t('pages.registries.kept') : '' }),
  };
  const where = h('div', { class: 'stack' });
  const paint = () => where.replaceChildren(...(dest === 's3'
    ? [h('div', { class: 'grid-2' }, f.endpoint.el, f.region.el, f.bucket.el, f.prefix.el, f.access_key.el, f.secret_key.el),
      settingRow({ id: 'vhost', label: t('pages.backups.vhost'), help: t('pages.backups.vhost_help'), control: toggle({ checked: virtualHost, labelledby: 'vhost-label', onChange: (v) => { virtualHost = v; } }) })]
    : [f.folder.el]));
  paint();
  const save = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true },
    settingRow({ id: 'backups-on', label: t('pages.backups.schedule'), help: t('pages.backups.schedule_help'), control: toggle({ checked: enabled, labelledby: 'backups-on-label', onChange: (v) => { enabled = v; } }) }),
    h('div', { class: 'grid-2' }, f.every_hours.el, f.keep.el), f.passphrase.el,
    h('div', { class: 'row' }, h('span', { class: 'muted' }, t('pages.notifications.to')), segmented({ label: t('pages.backups.destination'), size: 'sm', value: dest, options: [{ value: 'folder', label: t('pages.backups.a_folder') }, { value: 's3', label: 'S3' }], onChange: (v) => { dest = v; paint(); } })),
    where, h('div', { class: 'row row--end' }, save));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const x of Object.values(f)) x.setError('');
    save.disabled = true;
    try {
      const v = (k) => f[k].input.value.trim();
      await put('/api/settings/backups', { enabled, every_hours: Number(v('every_hours')), keep: Number(v('keep')), destination: dest, folder: v('folder'), passphrase: f.passphrase.input.value,
        s3: { endpoint: v('endpoint'), region: v('region'), bucket: v('bucket'), prefix: v('prefix'), access_key: v('access_key'), secret_key: v('secret_key'), virtual_host: virtualHost } });
      f.passphrase.input.value = ''; f.secret_key.input.value = '';
      toast({ text: t('common.saved'), tone: 'good' }); after();
    } catch (err) {
      const x = f[err.field];
      if (x) { x.setError(err.message); x.input.focus(); } else toast({ text: err.message, tone: 'critical' });
    } finally { save.disabled = false; }
  });
  return card({ title: t('nav.backups'), sub: t('pages.backups.settings_sub'), body: form });
}

/** After a restore: Freightdock starts again; wait for it, then load the page anew (the session may be another). */
function waitForRestart() {
  const m = openModal({ title: t('pages.backups.restoring'), body: h('div', { class: 'row' }, spinner(16), h('span', null, t('pages.backups.starting_again'))) });
  const started = Date.now();
  const poll = async () => {
    try { const r = await fetch('/api/health', { cache: 'no-store' }); if (r.ok && Date.now() - started > 1500) { m.close(); location.assign('/'); return; } } catch { /* still starting */ }
    setTimeout(poll, 1000);
  };
  setTimeout(poll, 1500);
}

function restoreDialog(name, label) {
  const pass = field('restore-pass', t('pages.backups.its_passphrase'), '', { type: 'password', autocomplete: 'off' });
  const go = button({ variant: 'danger', type: 'submit' }, t('pages.backups.restore'));
  const form = h('form', { class: 'stack', novalidate: true },
    callout({ tone: 'warning', body: t('pages.backups.restore_warning', { name: label }) }),
    pass.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.backups.restore_title'), body: form, initialFocus: pass.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); pass.setError('');
    go.disabled = true;
    try { await post('/api/backups/restore', { name, passphrase: pass.input.value }); m.close(); waitForRestart(); }
    catch (x) { pass.setError(x.message); go.disabled = false; }
  });
}

function uploadCard() {
  const rows = [];
  const drop = fileDrop({ label: t('pages.backups.drop'), help: t('pages.backups.drop_help'), accept: '.backup', maxSize: 1024 ** 3, onFiles: (taken, refused) => {
    rows.length = 0;
    for (const r of refused) rows.push({ name: r.file.name, size: r.file.size, error: r.reason });
    const f = taken[0];
    if (!f) { drop.show(rows); return; }
    const row = { name: f.name, size: f.size, progress: 0 };
    rows.push(row); drop.show(rows);
    const x = new XMLHttpRequest();
    x.open('POST', '/api/backups/upload');
    x.setRequestHeader('X-CSRF-Token', csrfToken());
    x.setRequestHeader('Content-Type', 'application/octet-stream');
    x.upload.addEventListener('progress', (e) => { if (e.lengthComputable) { row.progress = e.loaded / e.total; drop.show(rows); } });
    x.addEventListener('load', () => {
      if (x.status >= 200 && x.status < 300) { row.progress = 1; drop.show(rows); restoreDialog('uploaded', f.name); return; }
      let msg = t('api.answered', { status: String(x.status) });
      try { msg = JSON.parse(x.responseText).error || msg; } catch { /* not JSON */ }
      row.error = msg; drop.show(rows);
    });
    x.addEventListener('error', () => { row.error = t('pages.backups.unreachable'); drop.show(rows); });
    x.send(f);
  } });
  return card({ title: t('pages.backups.from_file'), body: h('div', { class: 'stack' }, drop,
    h('p', { class: 'muted' }, t('pages.backups.cli'))) });
}

function archivesCard(r, reload) {
  const st = r.status || {};
  const run = button({ size: 'sm', variant: 'primary', onClick: async () => {
    run.disabled = true;
    toast({ text: t('pages.backups.backing_up') });
    try { const b = await post('/api/backups/run'); toast({ text: t('pages.backups.made', { name: b.name, size: bytes(b.size) }), tone: 'good' }); reload(); }
    catch (e) { toast({ text: e.message, tone: 'critical' }); } finally { run.disabled = false; }
  } }, icon('save', 13), t('pages.backups.now'));
  const last = st.last_error ? callout({ tone: 'critical', title: t('pages.backups.last_failed', { when: ago(st.last_run_at) }), body: st.last_error })
    : st.last_run_at ? h('p', { class: 'muted' }, t('pages.backups.last', { name: st.last_name, size: bytes(st.last_size || 0), when: ago(st.last_run_at) })) : h('p', { class: 'muted' }, t('pages.backups.none_yet'));
  const list = r.error ? callout({ tone: 'critical', title: t('pages.backups.no_answer'), body: r.error }) : table({
    rows: r.archives, filter: false, empty: [t('pages.backups.no_archives'), t('pages.backups.no_archives_text')],
    columns: [
      { label: t('pages.backups.archive'), cell: (a) => h('span', { class: 'mono' }, a.name) },
      { label: t('pages.container.size'), cls: 'r', cell: (a) => h('span', { class: 'mono' }, bytes(a.size)), sort: (a) => a.size },
      { label: '', nosort: true, cell: (a) => h('div', { class: 'cell-actions' },
        button({ variant: 'icon', size: 'sm', href: `/api/backups/${encodeURIComponent(a.name)}/download`, download: '', 'aria-label': t('pages.backups.download', { name: a.name }) }, icon('download', 14)),
        button({ size: 'sm', onClick: async () => { if (await confirm({ title: t('pages.backups.restore_name', { name: a.name }), body: t('pages.backups.asked_next'), action: t('pages.events.go_on'), danger: false })) restoreDialog(a.name, a.name); } }, t('pages.backups.restore'))) },
    ],
  });
  return card({ title: r.destination === 's3' ? t('pages.backups.in_s3') : t('pages.backups.in_folder'), actions: run, body: h('div', { class: 'stack' }, last, list) });
}

export function backupsPage({ el, signal }) {
  const top = h('div'), archives = h('div');
  const reloadArchives = loadInto(archives, { signal, load: (s) => get('/api/backups', { signal: s }), draw: (r, again) => archivesCard(r, again) });
  loadInto(top, { signal, load: (s) => get('/api/settings/backups', { signal: s }), draw: (cfg) => settingsCard(cfg, reloadArchives) });
  el.append(pageHeader(t('nav.backups'), t('pages.backups.sub')), h('div', { class: 'stack' }, top, archives, uploadCard()));
}
