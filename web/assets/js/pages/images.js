// Images: what is here, how big, and what uses it; pulled with each layer's progress, built from a Dockerfile, a tar or
// a Git address with the build's own output, loaded from a file, exported, tagged, removed and pruned.

import { h, ago, when, bytes, shortId, mount } from '../dom.js';
import { get, post, del, qs } from '../api.js';
import { base, imageName, updateBadge } from '../docker.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { confirm } from '../confirm.js';
import { accessCard } from '../ownership.js';
import { navigate } from '../router.js';
import { postStream, layerTally } from '../stream.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { meter } from '../../finui/components/meter/meter.js';
import { callout } from '../../finui/components/callout/callout.js';
import { status } from '../../finui/components/badge/badge.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { outputView } from '../components/output-view/output-view.js';
import { codeEditor } from '../components/code-editor/code-editor.js';
import { t } from '../i18n.js';

/** The pull dialog: a reference in (`image` filled in, from the registry browser), each layer's progress out. A saved
 *  registry's sign-in goes with it by itself. */
export function pullDialog(after = () => {}, image = '') {
  const f = formField({ id: 'pull-image', label: t('pages.containers.image'), placeholder: t('pages.images.pull_placeholder'), autocomplete: 'off' });
  f.input.value = image;
  const go = button({ variant: 'primary', type: 'submit' }, icon('download', 14), t('pages.images.pull'));
  const out = h('div');
  const form = h('form', { class: 'stack', novalidate: true }, f.el, out, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.images.pull_title'), body: form, initialFocus: f.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const image = f.input.value.trim();
    f.setError(''); go.disabled = true;
    const tally = layerTally();
    const status = h('p', { class: 'muted mono' }, t('pages.images.asking'));
    const bar = h('div', null, meter({ indeterminate: true, block: true, label: t('pages.container_edit.pulling', { image }) }));
    mount(out, h('div', { class: 'stack' }, status, bar));
    try {
      await postStream(base('/images/pull'), { image }, (l) => {
        tally.add(l);
        if (l.status) status.textContent = l.id ? `${l.id}: ${l.status}` : l.status;
        const p = tally.percent;
        if (p != null) mount(bar, meter({ value: p, block: true, label: t('pages.container_edit.pulling', { image }) }));
      });
      m.close();
      toast({ text: t('pages.images.is_here', { image }), tone: 'good' });
      after();
    } catch (err) {
      out.replaceChildren();
      f.setError(err.message);
    } finally { go.disabled = false; }
  });
}

function loadDialog(after) {
  const file = h('input', { type: 'file', accept: '.tar,application/x-tar', class: 'fui-field__input', 'aria-label': t('pages.images.archive') });
  const go = button({ variant: 'primary', type: 'submit' }, icon('upload', 14), t('pages.images.load'));
  const log = outputView({ label: t('pages.images.load_output') });
  log.el.classList.add('fd-output--small');
  const out = h('div');
  const form = h('form', { class: 'stack', novalidate: true }, h('p', { class: 'muted' }, t('pages.images.load_help')), file, out, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.images.load_title'), body: form, wide: true });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    if (!file.files[0]) return;
    go.disabled = true; mount(out, log.el);
    try {
      await postStream(base('/images/load'), file.files[0], (l) => log.append('out', (l.stream || l.status || '') + (l.stream ? '' : '\n')), { raw: true, contentType: 'application/x-tar' });
      m.close(); toast({ text: t('pages.images.loaded'), tone: 'good' }); after();
    } catch (err) { log.append('err', err.message + '\n'); } finally { go.disabled = false; }
  });
}

function tagDialog(img, after) {
  const f = formField({ id: 'tag-ref', label: t('pages.images.new_tag'), placeholder: 'registry.example.com/team/app:1.0', autocomplete: 'off' });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.images.tag'));
  const form = h('form', { class: 'stack', novalidate: true }, f.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.images.tag_title', { name: imageName(img) }), body: form, initialFocus: f.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); f.setError('');
    try { await post(base(`/images/${encodeURIComponent(img.Id)}/tag`), { reference: f.input.value.trim() }); m.close(); toast({ text: t('pages.images.tagged'), tone: 'good' }); after(); } catch (err) { f.setError(err.message); }
  });
}

function pushDialog(img) {
  const tags = (img.RepoTags || []).filter((tag) => tag !== '<none>:<none>');
  const f = formField({ id: 'push-ref', label: t('pages.images.tag_to_push'), autocomplete: 'off' });
  f.input.value = tags[0] || '';
  const go = button({ variant: 'primary', type: 'submit' }, icon('upload', 14), t('pages.images.push'));
  const log = outputView({ label: t('pages.images.push_output') });
  log.el.classList.add('fd-output--small');
  const out = h('div');
  const form = h('form', { class: 'stack', novalidate: true }, f.el, h('p', { class: 'muted' }, t('pages.images.push_help')), out, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.images.push_title'), body: form, wide: true });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); go.disabled = true; mount(out, log.el);
    try {
      await postStream(base('/images/push'), { image: f.input.value.trim() }, (l) => log.append('out', `${l.id ? l.id + ': ' : ''}${l.status || ''} ${l.progress || ''}\n`));
      toast({ text: t('pages.images.pushed'), tone: 'good' }); m.close();
    } catch (err) { log.append('err', err.message + '\n'); } finally { go.disabled = false; }
  });
}

export async function removeImage(img, after = () => {}) {
  const used = img.containers > 0;
  const ok = await confirm({ title: t('common.remove_name', { name: imageName(img) }), body: used ? t('pages.images.remove_used', { count: img.containers }) : t('pages.images.remove_body'),
    option: used || (img.RepoTags || []).length > 1 ? { label: t('pages.images.force'), help: t('pages.images.force_help') } : null });
  if (!ok) return false;
  try { await del(base(`/images/${encodeURIComponent(img.Id)}${qs({ force: ok.option ? 1 : '' })}`)); toast({ text: t('common.removed'), tone: 'good' }); after(); return true; } catch (e) { toast({ text: e.message, tone: 'critical' }); return false; }
}

function menu(img, reload) {
  const items = [
    { label: t('pages.images.details'), icon: 'info', href: `/images/${encodeURIComponent(img.Id)}` },
    can('create') ? { label: t('pages.images.run_container'), icon: 'play', href: `/containers/new?image=${encodeURIComponent(imageName(img))}` } : null,
    can('administer') ? { label: t('pages.images.export'), icon: 'download', href: base(`/images/${encodeURIComponent(img.Id)}/export`) } : null,
  ];
  if (can('administer')) items.push({ separator: true }, { label: t('pages.images.tag'), icon: 'tag', onSelect: () => tagDialog(img, reload) }, { label: t('pages.images.push'), icon: 'upload', onSelect: () => pushDialog(img) },
    { separator: true }, { label: t('common.remove'), icon: 'trash', danger: true, onSelect: () => removeImage(img, reload) });
  return dropdownMenu({ label: t('common.actions_for', { name: imageName(img) }), items, icon: 'moreHorizontal', iconOnly: true, variant: 'ghost', size: 'sm' });
}

export function imagesPage({ el, signal }) {
  let words = [], updates = {};
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => {
      get(base('/updates'), { signal: s }).then((u) => { updates = u.images || {}; reload.redraw(); }, () => {});
      return get(base('/images'), { signal: s });
    },
    draw: (list, again) => table({
      rows: list.filter((i) => matches(words, ...(i.RepoTags || []), i.Id)).sort((a, b) => b.Created - a.Created), filter: false,
      empty: [t('pages.images.empty'), t('pages.images.empty_text')],
      columns: [
        { label: t('pages.containers.image'), cell: (i) => h('div', { class: 'name-cell' }, h('span', { class: 'row' }, h('a', { href: `/images/${encodeURIComponent(i.Id)}` }, imageName(i)), updateBadge(updates[i.Id])),
          (i.RepoTags || []).length > 1 ? h('span', { class: 'sub' }, t('pages.images.more_tags', { count: i.RepoTags.length - 1 })) : null) },
        { label: 'Id', cell: (i) => h('span', { class: 'mono faint' }, shortId(i.Id)) },
        { label: t('pages.volumes.used_by'), cell: (i) => (i.containers ? status({ tone: 'good', line: true }, t('common.n_containers', { count: i.containers })) : h('span', { class: 'faint' }, t('pages.volumes.nothing'))), sort: (i) => i.containers },
        { label: t('pages.container.size'), cls: 'r', cell: (i) => h('span', { class: 'mono' }, bytes(i.Size)), sort: (i) => i.Size },
        { label: t('common.made'), cell: (i) => h('span', { title: when(i.Created), class: 'nowrap' }, ago(i.Created)), sort: (i) => i.Created },
        { label: '', cell: (i) => h('div', { class: 'cell-actions' }, menu(i, again)), nosort: true },
      ],
    }),
  });
  const actions = h('div', { class: 'row' },
    can('administer') ? dropdownMenu({ label: t('common.clean_up'), size: 'sm', items: [
      { label: t('pages.images.untagged'), icon: 'trash', onSelect: async () => { if (await confirm({ title: t('pages.images.prune_untagged_title'), body: t('pages.images.prune_untagged_body') })) { try { const r = await post(base('/images/prune'), { all: false }); toast({ text: t('pages.images.freed', { size: bytes(r.SpaceReclaimed || 0) }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); } } } },
      { label: t('pages.images.every_unused'), icon: 'trash', danger: true, onSelect: async () => { if (await confirm({ title: t('pages.images.prune_all_title'), body: t('pages.images.prune_all_body') })) { try { const r = await post(base('/images/prune'), { all: true }); toast({ text: t('pages.images.freed', { size: bytes(r.SpaceReclaimed || 0) }), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); } } } },
    ] }) : null,
    can('create') ? button({ size: 'sm', onClick: () => loadDialog(reload) }, icon('upload', 13), t('pages.images.load')) : null,
    can('create') ? button({ size: 'sm', href: '/images/build' }, icon('package', 13), t('pages.images.build')) : null,
    can('create') ? button({ size: 'sm', variant: 'primary', onClick: () => pullDialog(reload) }, icon('download', 14), t('pages.images.pull')) : null);
  el.append(pageHeader(t('nav.images'), t('pages.images.sub'), actions), h('div', { class: 'toolbar' }, filterBox(t('pages.images.filter'), (w) => { words = w; reload.redraw(); })), slot);
}

export function imagePage({ el, params, signal }) {
  const slot = h('div');
  el.append(slot);
  const reload = loadInto(slot, {
    signal,
    load: async (s) => {
      const [d, list] = await Promise.all([get(base(`/images/${encodeURIComponent(params.id)}`), { signal: s }), get(base('/images'), { signal: s })]);
      return { ...d, summary: list.find((i) => i.Id === d.image.Id) || { Id: d.image.Id, RepoTags: d.image.RepoTags, containers: 0 } };
    },
    draw: ({ image, history, summary, freightdock_access }) => {
      const c = image.Config || {};
      const actions = h('div', { class: 'row' },
        can('create') ? button({ size: 'sm', href: `/containers/new?image=${encodeURIComponent(imageName(image))}` }, icon('play', 13), t('pages.images.run')) : null,
        can('administer') ? button({ size: 'sm', onClick: () => tagDialog(summary, reload) }, icon('tag', 13), t('pages.images.tag')) : null,
        can('administer') ? button({ size: 'sm', href: base(`/images/${encodeURIComponent(image.Id)}/export`) }, icon('download', 13), t('pages.images.export')) : null,
        can('administer') ? button({ size: 'sm', variant: 'danger', onClick: async () => { if (await removeImage(summary)) navigate('/images'); } }, icon('trash', 13), t('common.remove')) : null);
      return h('div', { class: 'stack' },
        pageHeader(imageName(image), `${shortId(image.Id)} · ${bytes(image.Size)}`, actions),
        card({ title: t('pages.containers.image'), body: facts([
          [t('pages.images.tags'), (image.RepoTags || []).join(', ') || t('common.none_word'), { wide: true, mono: true }],
          [t('pages.images.digest'), (image.RepoDigests || [])[0], { wide: true, mono: true }],
          [t('common.made'), when(Date.parse(image.Created) / 1000)], [t('pages.images.platform'), `${image.Os}/${image.Architecture}${image.Variant ? '/' + image.Variant : ''}`],
          [t('pages.volumes.used_by'), t('common.n_containers', { count: summary.containers })],
          [t('pages.container_edit.entrypoint'), (c.Entrypoint || []).join(' '), { mono: true }], [t('pages.container.command'), (c.Cmd || []).join(' '), { mono: true }],
          [t('pages.container_edit.workdir'), c.WorkingDir, { mono: true }], [t('nav.user'), c.User, { mono: true }],
          [t('pages.containers.ports'), Object.keys(c.ExposedPorts || {}).join(', '), { mono: true }], [t('nav.volumes'), Object.keys(c.Volumes || {}).join(', '), { mono: true }],
        ]) }),
        accessCard({ path: base(`/images/${encodeURIComponent(image.Id)}/access`), current: freightdock_access, reload }),
        card({ title: t('pages.container.environment'), body: h('pre', { class: 'mono pre' }, (c.Env || []).join('\n') || t('common.none')) }),
        card({ title: t('pages.images.layers'), sub: t('pages.images.steps', { count: history.length }), body: table({ rows: history, filter: false, dense: true, columns: [
          { label: t('pages.images.step'), cell: (l) => h('span', { class: 'mono pre-line' }, (l.CreatedBy || '').replace(/^\/bin\/sh -c (#\(nop\) )?/, '').trim()) },
          { label: t('pages.container.size'), cls: 'r', cell: (l) => h('span', { class: 'mono' }, bytes(l.Size)), sort: (l) => l.Size },
          { label: t('common.made'), cell: (l) => h('span', { class: 'nowrap' }, ago(l.Created)), sort: (l) => l.Created },
        ] }) }),
        card({ title: t('common.labels'), body: h('pre', { class: 'mono pre' }, Object.entries(c.Labels || {}).map(([k, v]) => `${k}=${v}`).join('\n') || t('common.none')) }));
    },
  });
}

const DOCKERFILE = 'FROM alpine:3.20\nRUN apk add --no-cache curl\nCMD ["sh", "-c", "echo hello from freightdock"]\n';

export function buildPage({ el }) {
  const tag = formField({ id: 'build-tag', label: t('pages.images.tag'), placeholder: 'myapp:latest', autocomplete: 'off' });
  const df = codeEditor({ id: 'build-df', label: 'Dockerfile', language: 'dockerfile', value: DOCKERFILE, rows: 16 });
  const dfField = h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: 'build-df' }, 'Dockerfile'), df.el);
  const file = h('input', { type: 'file', accept: '.tar,.tar.gz,.tgz,application/x-tar,application/gzip', class: 'fui-field__input', 'aria-label': t('pages.images.context') });
  const remote = formField({ id: 'build-remote', label: t('pages.images.git_address'), placeholder: 'https://github.com/owner/repo.git#main:path', help: t('pages.images.git_help') });
  const fileDf = formField({ id: 'build-dfpath', label: t('pages.images.dockerfile_inside'), placeholder: 'Dockerfile' });
  const gitDf = formField({ id: 'build-gitdf', label: t('pages.images.dockerfile_inside'), placeholder: 'Dockerfile' });
  let from = 'dockerfile', nocache = false, pull = false;
  const tabsEl = tabs({ label: t('pages.images.build_from'), value: from, onChange: (k) => { from = k; }, tabs: [
    { key: 'dockerfile', label: t('pages.images.a_dockerfile'), panel: dfField },
    { key: 'upload', label: t('pages.images.an_upload'), panel: h('div', { class: 'stack' }, h('p', { class: 'muted' }, t('pages.images.upload_help')), file, fileDf.el) },
    { key: 'git', label: 'Git', panel: h('div', { class: 'stack' }, remote.el, gitDf.el) },
  ] });
  const log = outputView({ label: t('pages.images.build_output') });
  const out = h('div');
  const go = button({ variant: 'primary', type: 'submit' }, icon('package', 14), t('pages.images.build'));
  const form = h('form', { class: 'stack', novalidate: true },
    card({ body: h('div', { class: 'stack' }, tag.el,
      settingRow({ id: 'nocache', label: t('pages.images.no_cache'), help: t('pages.images.no_cache_help'), control: toggle({ checked: false, labelledby: 'nocache-label', onChange: (v) => { nocache = v; } }) }),
      settingRow({ id: 'bpull', label: t('pages.images.pull_base'), help: t('pages.images.pull_base_help'), control: toggle({ checked: false, labelledby: 'bpull-label', onChange: (v) => { pull = v; } }) })) }),
    tabsEl, h('div', { class: 'row row--end' }, go), out);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    tag.setError(''); go.disabled = true; log.clear(); mount(out, log.el);
    const q = { tag: tag.input.value.trim(), nocache: nocache ? 1 : '', pull: pull ? 1 : '', dockerfile: fileDf.input.value.trim() };
    const onLine = (l) => { if (l.stream) log.append('out', l.stream); else if (l.status) log.append('out', `${l.id ? l.id + ': ' : ''}${l.status}\n`); };
    try {
      if (from === 'dockerfile') await postStream(base('/images/build' + qs({ ...q, dockerfile: '' })), { dockerfile: df.getValue() }, onLine);
      else if (from === 'upload') { if (!file.files[0]) throw new Error(t('pages.images.choose_file')); await postStream(base('/images/build' + qs(q)), file.files[0], onLine, { raw: true, contentType: 'application/x-tar' }); }
      else await postStream(base('/images/build' + qs({ ...q, dockerfile: gitDf.input.value.trim(), remote: remote.input.value.trim() })), null, onLine, { raw: true, contentType: 'application/octet-stream' });
      log.end(); log.note(t('pages.images.built'));
      toast({ text: q.tag ? t('pages.images.tag_built', { tag: q.tag }) : t('pages.images.built'), tone: 'good' });
    } catch (err) {
      log.end();
      if (err.field === 'tag') tag.setError(err.message); else log.append('err', err.message + '\n');
    } finally { go.disabled = false; }
  });
  el.append(pageHeader(t('routes.build'), t('pages.images.build_sub')), form);
}

export { callout };
