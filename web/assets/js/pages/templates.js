// App templates: Portainer's template lists (v2 and v3 JSON, by the addresses in Settings) and Freightdock's own custom
// templates, each deployed with the variables it asks for: a container template as a container, a stack template as a
// stack with Compose's output as it goes. Plus the custom template editor.

import { h, mount } from '../dom.js';
import { get, post, put, del } from '../api.js';
import { base } from '../docker.js';
import { loadInto, filterBox, matches } from '../view.js';
import { isAdmin, can } from '../state.js';
import { navigate } from '../router.js';
import { confirm } from '../confirm.js';
import { codeEditor } from '../editor.js';
import { runJob } from './stacks.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { formField, textArea } from '../../finui/components/field/field.js';
import { card } from '../../finui/components/card/card.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { callout } from '../../finui/components/callout/callout.js';
import { status } from '../../finui/components/badge/badge.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

/** Portainer's notes are HTML; here they are text ("UI" 2): tags dropped, the common entities read. */
export function noteText(html) {
  const ENT = { amp: '&', lt: '<', gt: '>', quot: '"', apos: "'", nbsp: ' ' };
  return String(html || '').replace(/<br\s*\/?>/gi, '\n').replace(/<[^>]*>/g, '').replace(/&(#\d+|[a-z]+);/gi, (m, e) => (e[0] === '#' ? String.fromCharCode(Number(e.slice(1))) : ENT[e.toLowerCase()] ?? m)).trim();
}

const KIND = { container: ['info', t('pages.templates.kind.container')], stack: ['good', t('pages.templates.kind.stack')], swarm: ['warning', t('nav.swarm')] };

function logo(tpl) {
  return tpl.logo ? h('img', { class: 'tpl-logo', src: tpl.logo, alt: '', loading: 'lazy', referrerPolicy: 'no-referrer', decoding: 'async' }) : h('span', { class: 'tpl-logo tpl-logo--none' }, icon(tpl.kind === 'container' ? 'package' : 'grid', 18));
}

/** The variables a template asks for, as fields: a choice as a select, the rest as text; preset ones are not asked. */
function varFields(vars) {
  const asked = (vars || []).filter((v) => !v.preset);
  const fields = asked.map((v) => {
    const id = `tpl-var-${v.name}`;
    const label = v.label || v.name;
    if (v.select && v.select.length) {
      const sel = h('select', { class: 'fui-field__input', id }, v.select.map((c) => h('option', { value: c.value, selected: !!c.default }, c.text || c.value)));
      return { name: v.name, el: h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: id }, label), sel, v.description ? h('p', { class: 'fui-field__help' }, v.description) : null), value: () => sel.value };
    }
    const f = formField({ id, label, help: v.description || (label !== v.name ? v.name : null), autocomplete: 'off' });
    f.input.value = v.default || '';
    return { name: v.name, el: f.el, value: () => f.input.value };
  });
  return { el: fields.length ? h('div', { class: 'stack' }, fields.map((f) => f.el)) : null, values: () => Object.fromEntries(fields.map((f) => [f.name, f.value()])) };
}

/** The deploy dialog: what the template is, its name and variables, and the deploy with its outcome. */
function deployDialog(tpl, which) {
  const note = noteText(tpl.note);
  const name = formField({ id: 'tpl-name', label: t('common.name'), autocomplete: 'off',
    help: tpl.kind === 'container' ? t('pages.templates.name_container') : t('pages.templates.name_stack') });
  name.input.value = tpl.kind === 'container' ? (tpl.name || '') : tpl.title.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '').slice(0, 63);
  const vars = varFields(tpl.env);
  const out = h('div');
  const go = button({ variant: 'primary', type: 'submit' }, icon('rocket', 14), t('pages.stack.deploy'));
  const what = tpl.kind === 'container' ? (tpl.ports && tpl.ports.length ? t('pages.templates.runs_ports', { image: tpl.image, ports: tpl.ports.join(', ') }) : t('pages.templates.runs', { image: tpl.image }))
    : tpl.repository ? t('pages.templates.git_stack', { url: tpl.repository.url, file: tpl.repository.stackfile }) : tpl.stackfile ? t('pages.templates.stack_from', { file: tpl.stackfile }) : t('pages.templates.stack_compose');
  const form = h('form', { class: 'stack', novalidate: true },
    h('p', { class: 'muted' }, tpl.description), note ? callout({ tone: 'info', body: h('span', { class: 'pre-line' }, note) }) : null,
    h('p', { class: 'muted' }, what), name.el, vars.el, out, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: tpl.title, body: form, wide: true, initialFocus: name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); name.setError(''); go.disabled = true;
    try {
      const r = await post(base('/templates/deploy'), { ...which, name: name.input.value.trim(), env: vars.values() });
      if (r.container) { m.close(); toast({ text: t('pages.templates.running', { name: tpl.title }), tone: 'good' }); navigate(`/containers/${encodeURIComponent(r.container)}`); return; }
      await runJob(r.stack, 'deploy', {}, out, { quiet: true });
      m.close(); navigate(`/stacks/${r.stack}`);
    } catch (err) {
      if (err.field === 'name') name.setError(err.message); else mount(out, callout({ tone: 'critical', body: err.message }));
    } finally { go.disabled = false; }
  });
}

function tile(tpl, which) {
  const [tone, word] = KIND[tpl.kind] || KIND.container;
  const may = can('create') && tpl.deployable;
  return h('div', { class: 'fui-card tpl-card' },
    h('div', { class: 'tpl-head' }, logo(tpl), h('div', { class: 'name-cell' }, h('strong', null, tpl.title), h('span', { class: 'sub' }, (tpl.categories || []).join(', ')))),
    h('p', { class: 'muted tpl-desc' }, tpl.description),
    h('div', { class: 'row tpl-foot' }, status({ tone, line: true }, word),
      tpl.deployable ? null : h('span', { class: 'faint' }, tpl.kind === 'swarm' ? t('pages.templates.needs_swarm') : t('pages.templates.nothing_to_deploy')),
      h('span', { class: 'tpl-spacer' }),
      may ? button({ size: 'sm', onClick: () => deployDialog(tpl, which) }, t('pages.stack.deploy')) : null));
}

function grid(items, words, empty) {
  const shown = items.filter(({ tpl }) => matches(words, tpl.title, tpl.description, ...(tpl.categories || []), tpl.image));
  return shown.length ? h('div', { class: 'tpl-grid' }, shown.map(({ tpl, which, extra }) => extra ? h('div', { class: 'tpl-cell' }, tile(tpl, which), extra) : tile(tpl, which))) : emptyState(...empty);
}

export function templatesPage({ el, signal, query }) {
  let words = [], tab = query.get('tab') === 'custom' ? 'custom' : 'lists', refresh = false;
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => { const r = get(`/api/templates${refresh ? '?refresh=true' : ''}`, { signal: s }); refresh = false; return r; },
    draw: (d) => {
      const fromLists = d.lists.flatMap((l) => l.templates.map((tpl, index) => ({ tpl, which: { list: l.url, index } })));
      const errors = d.lists.filter((l) => l.error).map((l) => callout({ tone: 'warning', title: t('pages.templates.list_unreadable'), body: l.error }));
      const custom = d.custom.map((c) => ({ tpl: { ...c, kind: 'stack', deployable: true, env: c.env }, which: { custom: c.id },
        extra: isAdmin() ? h('div', { class: 'row tpl-edit' }, button({ size: 'sm', variant: 'ghost', href: `/templates/custom/${c.id}` }, icon('pencil', 13), t('common.edit'))) : null }));
      return tabs({ label: t('nav.templates'), value: tab, onChange: (k) => { tab = k; history.replaceState(null, '', k === 'custom' ? '?tab=custom' : location.pathname); }, tabs: [
        { key: 'lists', label: t('pages.templates.app_templates'), count: fromLists.length, panel: () => h('div', { class: 'stack' }, errors,
          d.lists.length ? grid(fromLists, words, [t('pages.templates.no_match'), t('pages.templates.other_words')]) : emptyState(t('pages.templates.no_lists'), t('pages.templates.no_lists_text'))) },
        { key: 'custom', label: t('pages.templates.custom'), count: custom.length, panel: () => grid(custom, words, [t('pages.templates.no_custom'), t('pages.templates.no_custom_text')]) },
      ] });
    },
  });
  const actions = h('div', { class: 'row' },
    button({ size: 'sm', onClick: () => { refresh = true; reload(); } }, icon('refresh', 13), t('pages.templates.read_again')),
    isAdmin() ? button({ size: 'sm', variant: 'primary', href: '/templates/custom/new' }, icon('plus', 14), t('routes.new_template')) : null);
  el.append(pageHeader(t('nav.templates'), t('pages.templates.sub'), actions),
    h('div', { class: 'toolbar' }, filterBox(t('pages.templates.filter'), (w) => { words = w; reload.redraw(); })), slot);
}

/** Variables as the editor writes them, NAME=default a line, a `# words` line before one its label. */
export function varsToText(env) {
  return (env || []).map((v) => `${v.label && v.label !== v.name ? `# ${v.label}\n` : ''}${v.name}=${v.default || ''}`).join('\n');
}
export function textToVars(text) {
  const out = [];
  let label = '';
  for (const raw of String(text).split('\n')) {
    const l = raw.trim();
    if (!l) { label = ''; continue; }
    if (l.startsWith('#')) { label = l.replace(/^#+\s*/, ''); continue; }
    const i = l.indexOf('=');
    const name = (i < 0 ? l : l.slice(0, i)).trim();
    out.push({ name, label: label || name, default: i < 0 ? '' : l.slice(i + 1) });
    label = '';
  }
  return out;
}

export function customTemplatePage({ el, params, signal }) {
  const isNew = !params.id;
  const slot = h('div');
  el.append(slot);
  const draw = (c) => {
    const title = formField({ id: 'ct-title', label: t('pages.templates.title'), autocomplete: 'off' });
    const desc = formField({ id: 'ct-desc', label: t('pages.templates.description'), autocomplete: 'off' });
    const note = textArea({ id: 'ct-note', label: t('pages.templates.note'), rows: 2, help: t('pages.templates.note_help') });
    const cats = formField({ id: 'ct-cats', label: t('pages.templates.categories'), help: t('pages.templates.categories_help'), autocomplete: 'off' });
    const logoF = formField({ id: 'ct-logo', label: t('pages.templates.logo'), placeholder: 'https://example.com/logo.png', autocomplete: 'off' });
    const file = codeEditor({ id: 'ct-compose', label: t('pages.stacks.compose_file'), value: c.compose || '', rows: 18, language: 'yaml' });
    const vars = codeEditor({ id: 'ct-env', label: t('pages.templates.vars_asked'), value: varsToText(c.env), rows: 6, language: 'env',
      help: t('pages.templates.vars_help') });
    title.input.value = c.title || ''; desc.input.value = c.description || ''; note.input.value = c.note || ''; cats.input.value = (c.categories || []).join(', '); logoF.input.value = c.logo || '';
    const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
    const remove = isNew ? null : button({ variant: 'danger', onClick: async () => {
      if (!(await confirm({ title: t('common.remove_name', { name: c.title }), body: t('pages.templates.remove_body') }))) return;
      try { await del(`/api/templates/custom/${params.id}`); toast({ text: t('common.removed'), tone: 'good' }); navigate('/templates?tab=custom'); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
    } }, icon('trash', 13), t('common.remove'));
    const form = h('form', { class: 'stack', novalidate: true },
      card({ body: h('div', { class: 'stack' }, title.el, desc.el, note.el, h('div', { class: 'grid-2' }, cats.el, logoF.el)) }),
      card({ body: h('div', { class: 'stack' }, file.el, vars.el) }), h('div', { class: 'row row--end' }, remove, go));
    form.addEventListener('submit', async (e) => {
      e.preventDefault(); [title, file, vars].forEach((f) => f.setError('')); go.disabled = true;
      const body = { title: title.input.value.trim(), description: desc.input.value.trim(), note: note.input.value.trim(), logo: logoF.input.value.trim(),
        categories: cats.input.value.split(',').map((x) => x.trim()).filter(Boolean), compose: file.value, env: textToVars(vars.value) };
      try {
        if (isNew) { const r = await post('/api/templates/custom', body); toast({ text: t('common.saved'), tone: 'good' }); navigate(`/templates/custom/${r.id}`); }
        else { await put(`/api/templates/custom/${params.id}`, body); toast({ text: t('common.saved'), tone: 'good' }); }
      } catch (err) {
        ({ title, compose: file, env: vars }[err.field] || { setError: (m) => toast({ text: m, tone: 'critical' }) }).setError(err.message);
      } finally { go.disabled = false; }
    });
    return h('div', { class: 'stack' }, pageHeader(isNew ? t('routes.new_template') : c.title, t('pages.templates.custom_sub')), form);
  };
  if (isNew) mount(slot, draw({}));
  else loadInto(slot, { signal, load: (s) => get(`/api/templates/custom/${params.id}`, { signal: s }), draw });
}
