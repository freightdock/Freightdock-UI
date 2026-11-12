// Stacks: Freightdock's Compose stacks and every project started elsewhere (external, to adopt), a new stack written in the
// editor, and a project from elsewhere looked at before it is adopted.

import { accessPicker } from '../ownership.js';
import { h, ago, when, mount } from '../dom.js';
import { get, post } from '../api.js';
import { base, containerName, stateBadge } from '../docker.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { navigate } from '../router.js';
import { postStream } from '../stream.js';
import { codeEditor, ENV_HELP } from '../editor.js';
import { gitFields } from './stack-git.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { formField } from '../../finui/components/field/field.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { callout } from '../../finui/components/callout/callout.js';
import { status } from '../../finui/components/badge/badge.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { outputView } from '../components/output-view/output-view.js';
import { t } from '../i18n.js';

const TONES = { running: 'good', partial: 'warning', stopped: 'critical', down: 'info' };
const WORDS = { running: t('pages.stacks.state.running'), partial: t('pages.stacks.state.partial'), stopped: t('pages.stacks.state.stopped'), down: t('pages.stacks.state.down'), unknown: t('pages.stacks.state.unknown') };

/** A stack's state as FinUI's status badge. */
export const stackState = (s) => status({ tone: TONES[s] || 'info', line: true, class: 'nowrap' }, WORDS[s] || s);


/**
 * Runs a stack job (deploy, stop, start, pull, remove) with Compose's own output shown in `out` as it comes. Answers
 * true when it ended well.
 */
export async function runJob(id, word, body, out, { quiet = false, onError } = {}) {
  const view = outputView({ label: t('pages.stacks.compose_output') });
  mount(out, card({ title: 'Compose', body: view.el }));
  try {
    await postStream(base(`/stacks/${id}/${word}`), body || {}, (l) => { if (l.stream) view.append('out', l.stream); });
    view.end();
    if (!quiet) toast({ text: t('common.done'), tone: 'good' });
    return true;
  } catch (e) {
    view.end();
    view.append('err', e.message + '\n');
    toast({ text: e.message, tone: 'critical' });
    if (onError) onError(e);
    return false;
  }
}

export function stacksPage({ el, signal, every }) {
  let words = [];
  const slot = h('div');
  const href = (s) => (s.external ? `/stacks/external/${encodeURIComponent(s.name)}` : `/stacks/${s.id}`);
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(base('/stacks'), { signal: s }),
    draw: (list) => table({
      rows: list.filter((s) => matches(words, s.name, s.source, ...(s.services || []))), filter: false,
      empty: [t('pages.stacks.empty'), t('pages.stacks.empty_text')],
      rowClass: (s) => (s.state !== 'running' ? 'is-quiet' : null),
      columns: [
        { label: t('common.name'), cell: (s) => h('div', { class: 'name-cell' }, h('a', { href: href(s) }, s.name), h('span', { class: 'sub' }, (s.services || []).join(', '))) },
        { label: t('common.state'), cell: (s) => h('div', { class: 'name-cell' }, stackState(s.state), h('span', { class: 'sub' }, s.containers ? t('pages.stacks.n_of_running', { running: s.running, count: s.containers }) : '')), sort: (s) => s.state },
        { label: t('pages.stacks.from'), cell: (s) => (s.external ? h('span', { class: 'row' }, status({ tone: 'warning', line: true }, t('pages.stacks.external')), s.portainer ? h('span', { class: 'faint' }, 'Portainer') : null)
          : s.source === 'git' ? h('span', { class: 'row' }, icon('github', 13), 'Git') : t('pages.stacks.editor')), sort: (s) => s.source },
        { label: t('pages.stacks.deployed'), cell: (s) => (s.deployed_at ? h('span', { class: 'nowrap', title: when(s.deployed_at) }, ago(s.deployed_at)) : h('span', { class: 'faint' }, s.external ? '' : t('common.never'))), sort: (s) => s.deployed_at || 0 },
        { label: '', nosort: true, cell: (s) => (s.external && can('administer') ? h('div', { class: 'cell-actions' }, button({ size: 'sm', href: href(s) }, t('pages.stacks.adopt'))) : '') },
      ],
    }),
  });
  const actions = can('create') ? h('div', { class: 'row' }, button({ size: 'sm', variant: 'primary', href: '/stacks/new' }, icon('plus', 14), t('pages.stacks.add'))) : null;
  el.append(pageHeader(t('nav.stacks'), t('pages.stacks.sub'), actions),
    h('div', { class: 'toolbar' }, filterBox(t('pages.stacks.filter'), (w) => { words = w; reload.redraw(); })), slot);
  every(5000, reload);
}

/** The editor's two boxes, the compose file and its variables. */
export function stackFiles({ compose = '', env = '' } = {}) {
  const file = codeEditor({ id: 'stack-compose', label: t('pages.stacks.compose_file'), value: compose, rows: 20, language: 'yaml' });
  const vars = codeEditor({ id: 'stack-env', label: t('pages.stacks.variables'), value: env, rows: 6, help: ENV_HELP, language: 'env' });
  return { file, vars, el: h('div', { class: 'stack' }, file.el, vars.el) };
}

/** Puts an answer's error by the box it is about. */
export function showError(err, { name, file, vars }) {
  if (err.field === 'name' && name) name.setError(err.message);
  else if (err.field === 'env') vars.setError(err.message);
  else if (err.field === 'compose') file.setError(err.message);
  else toast({ text: err.message, tone: 'critical' });
}

export function newStackPage({ el }) {
  const name = formField({ id: 'stack-name', label: t('common.name'), placeholder: 'my-app', autocomplete: 'off', help: t('pages.stacks.name_help') });
  const files = stackFiles();
  const repo = gitFields();
  const access = accessPicker({ id: 'stack-access' });
  let deploy = true, from = 'editor';
  const out = h('div');
  const go = button({ variant: 'primary', type: 'submit' }, icon('rocket', 14), t('common.create'));
  const form = h('form', { class: 'stack', novalidate: true },
    card({ body: h('div', { class: 'stack' }, name.el, access.el,
      settingRow({ id: 'stack-deploy', label: t('pages.stacks.deploy_now'), help: t('pages.stacks.deploy_now_help'), control: toggle({ checked: true, labelledby: 'stack-deploy-label', onChange: (v) => { deploy = v; } }) })) }),
    tabs({ label: t('pages.stacks.where_from'), value: from, onChange: (k) => { from = k; }, tabs: [
      { key: 'editor', label: t('pages.stacks.written_here'), panel: card({ body: files.el }) },
      { key: 'git', label: t('pages.stacks.a_git_repo'), panel: card({ body: repo.el }) },
    ] }),
    h('div', { class: 'row row--end' }, go), out);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    name.setError(''); files.file.setError(''); files.vars.setError(''); repo.clear();
    go.disabled = true;
    try {
      const r = from === 'git'
        ? await post(base('/stacks/git'), { name: name.input.value.trim(), ...repo.value(), access: access.value() }).catch((err) => { if (err.field !== 'name' && repo.setError(err)) err.shown = true; throw err; })
        : await post(base('/stacks'), { name: name.input.value.trim(), compose: files.file.value, env: files.vars.value, access: access.value() });
      if (deploy) await runJob(r.id, 'deploy', {}, out);
      else toast({ text: t('pages.stacks.kept'), tone: 'good' });
      navigate(`/stacks/${r.id}`);
    } catch (err) { if (!err.shown) showError(err, { name, ...files }); } finally { go.disabled = false; }
  });
  el.append(pageHeader(t('routes.add_stack'), t('pages.stacks.new_sub')), form);
}

/** A project started elsewhere: what Docker knows of it, and adopting it. */
export function externalStackPage({ el, params, signal }) {
  const slot = h('div');
  el.append(slot);
  loadInto(slot, {
    signal,
    load: (s) => get(base(`/stacks/external/${encodeURIComponent(params.name)}`), { signal: s }),
    draw: (x) => {
      const p = x.project;
      const files = stackFiles({ compose: x.compose || '', env: x.env || '' });
      const go = button({ variant: 'primary', type: 'submit' }, icon('download', 14), t('pages.stacks.adopt'));
      const form = h('form', { class: 'stack', novalidate: true },
        x.unreadable ? callout({ tone: 'warning', title: t('pages.stacks.cannot_read'), body: t('pages.stacks.unreadable', { reason: x.unreadable }) })
          : callout({ tone: 'info', body: t('pages.stacks.adopt_help') }),
        card({ body: files.el }), h('div', { class: 'row row--end' }, go));
      form.addEventListener('submit', async (e) => {
        e.preventDefault();
        files.file.setError(''); files.vars.setError('');
        go.disabled = true;
        try {
          const r = await post(base('/stacks/adopt'), { name: p.name, compose: files.file.value, env: files.vars.value });
          toast({ text: t('pages.stacks.adopted', { name: p.name }), tone: 'good' });
          navigate(`/stacks/${r.id}`);
        } catch (err) { showError(err, files); } finally { go.disabled = false; }
      });
      return h('div', { class: 'stack' },
        pageHeader(h('span', { class: 'row' }, p.name, stackState(x.state), status({ tone: 'warning', line: true }, t('pages.stacks.external'))), t('pages.stacks.not_started_by')),
        card({ title: t('pages.stacks.project'), body: facts([
          [t('nav.services'), (p.services || []).join(', ')], [t('nav.containers'), t('pages.stacks.n_of_running', { running: p.running, count: p.containers })],
          [t('pages.stacks.started_from'), p.working_dir, { wide: true, mono: true }], [t('pages.stacks.files'), (p.config_files || []).join('\n'), { wide: true, mono: true }],
          [t('pages.stacks.started_by'), p.portainer ? 'Portainer' : null],
        ]) }),
        card({ title: t('nav.containers'), body: table({ rows: x.containers, filter: false, dense: true, columns: [
          { label: t('common.name'), cell: (c) => h('a', { href: `/containers/${encodeURIComponent(containerName(c))}` }, containerName(c)) },
          { label: t('common.state'), cell: (c) => stateBadge(c.State) },
          { label: t('pages.containers.image'), cell: (c) => h('span', { class: 'mono truncate' }, c.Image) },
        ] }) }),
        can('administer') ? form : null);
    },
  });
}

