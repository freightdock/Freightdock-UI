// Registries: where images come from and go to, and how Freightdock signs in there. Everybody sees them and browses them
// (repositories, then a repository's tags); an administrator adds one (tested before it is saved), edits, removes, and
// pulls a tag onto the host shown (only while one is chosen: "UI" 12). The password is never shown again: the form
// says only whether there is one.

import { h, mount } from '../dom.js';
import { get, post, put, del, qs } from '../api.js';
import { loadInto, table } from '../view.js';
import { isAdmin, currentEnv } from '../state.js';
import { confirm } from '../confirm.js';
import { pullDialog } from './images.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { callout } from '../../finui/components/callout/callout.js';
import { card } from '../../finui/components/card/card.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { errorState } from '../../finui/components/error/error.js';
import { toast } from '../../finui/components/toast/toast.js';
import { sk } from '../../finui/components/skeleton/skeleton.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

/** Each kind: its name, the address it has unless one is given, and what its sign-in is called. */
const KINDS = {
  dockerhub: { label: 'Docker Hub', url: 'docker.io', user: t('auth.user_name'), secret: t('pages.registries.secret_dockerhub'), help: t('pages.registries.help_dockerhub') },
  ghcr: { label: 'GitHub (ghcr.io)', url: 'ghcr.io', user: t('pages.registries.user_ghcr'), secret: t('pages.registries.secret_token'), help: t('pages.registries.help_ghcr') },
  gitlab: { label: 'GitLab', url: 'registry.gitlab.com', user: t('auth.user_name'), secret: t('pages.registries.secret_gitlab'), help: t('pages.registries.help_gitlab') },
  ecr: { label: 'Amazon ECR', url: '', user: t('pages.registries.user_ecr'), secret: t('pages.registries.secret_ecr'), help: t('pages.registries.help_ecr') },
  quay: { label: 'Quay', url: 'quay.io', user: t('pages.registries.user_quay'), secret: t('pages.registries.secret_password_token'), help: t('pages.registries.help_quay') },
  forgejo: { label: 'Forgejo', url: '', placeholder: 'git.example.com', user: t('pages.registries.user_forgejo'), secret: t('pages.registries.secret_access_token'), help: t('pages.registries.help_forgejo') },
  gitea: { label: 'Gitea', url: '', placeholder: 'gitea.example.com', user: t('pages.registries.user_gitea'), secret: t('pages.registries.secret_access_token'), help: t('pages.registries.help_gitea') },
  custom: { label: t('pages.registries.another'), url: '', user: t('auth.user_name'), secret: t('pages.registries.secret_password_token'), help: t('pages.registries.help_custom') },
};

const kindLabel = (k) => (KINDS[k] || {}).label || k;

/** The form for a registry: a new one, or `reg` to edit. */
function registryDialog(reg, after) {
  let kind = reg ? reg.kind : 'dockerhub';
  const name = formField({ id: 'reg-name', label: t('common.name'), placeholder: 'Docker Hub', autocomplete: 'off' });
  const url = formField({ id: 'reg-url', label: t('pages.environments.address'), autocomplete: 'off', help: t('pages.registries.address_help') });
  const username = formField({ id: 'reg-user', label: t('auth.user_name'), autocomplete: 'off' });
  const password = formField({ id: 'reg-password', label: t('auth.password'), type: 'password', autocomplete: 'new-password' });
  const region = formField({ id: 'reg-region', label: t('pages.registries.region'), placeholder: 'eu-north-1', autocomplete: 'off', help: t('pages.registries.region_help') });
  const about = h('p', { class: 'faint' });
  const select = h('select', { class: 'fui-field__input', id: 'reg-kind', disabled: !!reg },
    Object.entries(KINDS).map(([k, v]) => h('option', { value: k, selected: k === kind }, v.label)));
  if (reg) {
    name.input.value = reg.name;
    url.input.value = reg.url;
    username.input.value = reg.username;
    region.input.value = reg.region || '';
  }
  const paint = () => {
    const k = KINDS[kind];
    url.el.hidden = kind === 'dockerhub';
    region.el.hidden = kind !== 'ecr';
    url.input.placeholder = k.url || k.placeholder || (kind === 'ecr' ? '123456789012.dkr.ecr.eu-north-1.amazonaws.com' : 'registry.example.com');
    username.el.querySelector('label').textContent = k.user;
    password.el.querySelector('label').textContent = k.secret;
    password.input.placeholder = reg && reg.has_secret ? t('pages.registries.kept') : '';
    about.textContent = k.help;
    if (!reg && !name.input.dataset.typed) name.input.value = k.label;
  };
  name.input.addEventListener('input', () => { name.input.dataset.typed = '1'; });
  select.addEventListener('change', () => { kind = select.value; paint(); });
  paint();
  const go = button({ variant: 'primary', type: 'submit' }, reg ? t('pages.registries.test_save') : t('pages.registries.test_add'));
  const kindField = h('div', { class: 'fui-field' }, h('label', { htmlFor: 'reg-kind', class: 'fui-field__label' }, t('pages.networks.kind')), select);
  const form = h('form', { class: 'stack', novalidate: true }, kindField, about, name.el, url.el, username.el, password.el, region.el,
    h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: reg ? t('routes.edit', { name: reg.name }) : t('pages.registries.add_title'), body: form, initialFocus: reg ? name.input : select });
  const fields = { name, url, username, password, region };
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const f of Object.values(fields)) f.setError('');
    go.disabled = true;
    const body = { name: name.input.value, kind, url: url.input.value, username: username.input.value, password: password.input.value, region: region.input.value };
    try {
      if (reg) await put(`/api/registries/${reg.id}`, body); else await post('/api/registries', body);
      m.close();
      toast({ text: reg ? t('common.saved') : t('pages.registries.added'), tone: 'good' });
      after();
    } catch (err) {
      const f = err.field && fields[err.field];
      if (f) { f.setError(err.message); f.input.focus(); } else name.setError(err.message);
    } finally { go.disabled = false; }
  });
}

async function remove(reg, after) {
  const ok = await confirm({ title: t('common.remove_name', { name: reg.name }), body: t('pages.registries.remove_body') });
  if (!ok) return;
  try { await del(`/api/registries/${reg.id}`); toast({ text: t('common.removed'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

export function registriesPage({ el, signal }) {
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get('/api/registries', { signal: s }),
    draw: (list, again) => table({
      rows: list, empty: [t('pages.registries.empty'), isAdmin() ? t('pages.registries.empty_admin') : t('pages.registries.empty_user')],
      columns: [
        { label: t('common.name'), cell: (r) => h('a', { href: `/registries/${r.id}`, class: 'truncate' }, r.name) },
        { label: t('pages.networks.kind'), cell: (r) => kindLabel(r.kind) },
        { label: t('pages.environments.address'), cell: (r) => h('span', { class: 'mono truncate' }, r.url) },
        { label: t('pages.registries.signed_in_as'), cell: (r) => (r.username ? h('span', { class: 'mono truncate' }, r.username) : h('span', { class: 'faint' }, t('pages.registries.nobody'))) },
        { label: '', nosort: true, cell: (r) => h('div', { class: 'cell-actions' },
          button({ size: 'sm', href: `/registries/${r.id}` }, t('pages.registries.browse')),
          isAdmin() ? button({ variant: 'icon', size: 'sm', 'aria-label': t('routes.edit', { name: r.name }), onClick: () => registryDialog(r, again) }, icon('settings', 14)) : null,
          isAdmin() ? button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: r.name }), onClick: () => remove(r, again) }, icon('trash', 14)) : null) },
      ],
    }),
  });
  const actions = isAdmin() ? button({ size: 'sm', variant: 'primary', onClick: () => registryDialog(null, reload) }, icon('plus', 14), t('pages.registries.add')) : null;
  el.append(pageHeader(t('nav.registries'), t('pages.registries.sub'), actions), slot);
}

/** A failure drawn in place, with Try again. */
function failed(slot, e, again) { mount(slot, errorState(e, again)); }

/** The registry browser: its repositories (or a name typed in), then the tags of one, each to pull. */
export function registryPage({ el, params, signal }) {
  const id = params.id;
  const head = h('div');
  const repoSlot = h('div');
  const tagSlot = h('div');
  el.append(head, repoSlot, tagSlot);
  let reg = null;

  const showTags = async (repository) => {
    mount(tagSlot, sk.tableRows(4));
    try {
      const r = await get(`/api/registries/${id}/tags${qs({ repository })}`, { signal });
      const env = currentEnv();
      mount(tagSlot, card({ title: r.image, sub: t('pages.registries.n_tags', { count: r.tags.length }), body: table({
        rows: r.tags.map((row) => ({ tag: row, ref: `${r.image}:${row}` })), empty: [t('pages.env_groups.no_tags'), t('pages.registries.no_tags_text')],
        columns: [
          { label: t('common.tag'), cell: (row) => h('span', { class: 'mono' }, row.tag) },
          { label: t('pages.containers.image'), cell: (row) => h('span', { class: 'mono truncate' }, row.ref) },
          { label: '', nosort: true, cell: (row) => (isAdmin() && env ? h('div', { class: 'cell-actions' },
            button({ size: 'sm', title: t('pages.registries.pull_onto', { name: env.name }), onClick: () => pullDialog(() => {}, row.ref) }, icon('download', 14), t('pages.images.pull'))) : null) },
        ],
      }) }));
      tagSlot.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    } catch (e) {
      if (!signal.aborted) failed(tagSlot, e, () => showTags(repository));
    }
  };

  // A repository's name typed in: for registries that list none, or one not listed.
  const repoField = formField({ id: 'reg-repo', label: t('pages.stack_git.repository'), placeholder: 'owner/app', autocomplete: 'off' });
  const byName = h('form', { class: 'row row--bottom', novalidate: true }, repoField.el, button({ type: 'submit' }, t('pages.registries.show_tags')));
  byName.addEventListener('submit', (e) => { e.preventDefault(); const v = repoField.input.value.trim(); if (v) showTags(v); });

  // Docker Hub: whose repositories.
  const nsField = formField({ id: 'reg-ns', label: t('pages.registries.account_or_org'), autocomplete: 'off' });
  const byNs = h('form', { class: 'row row--bottom', novalidate: true }, nsField.el, button({ type: 'submit' }, t('pages.registries.list_repos')));
  byNs.addEventListener('submit', (e) => { e.preventDefault(); listRepos(); });

  const listRepos = async () => {
    mount(repoSlot, sk.tableRows(5));
    try {
      const q = reg.kind === 'dockerhub' && nsField.input.value.trim() ? qs({ namespace: nsField.input.value.trim() }) : '';
      const r = await get(`/api/registries/${id}/repositories${q}`, { signal });
      const body = r.listed
        ? table({
          rows: r.repositories.map((name) => ({ name })), label: t('pages.registries.filter_repos'),
          empty: [t('pages.registries.no_repos'), t('pages.registries.no_repos_text')],
          columns: [
            { label: t('pages.stack_git.repository'), cell: (x) => h('span', { class: 'mono truncate' }, x.name) },
            { label: '', nosort: true, cell: (x) => h('div', { class: 'cell-actions' }, button({ size: 'sm', onClick: () => showTags(x.name) }, t('pages.images.tags'))) },
          ],
        })
        : callout({ tone: 'info', body: r.reason });
      mount(repoSlot, card({ title: t('pages.registries.repositories'), sub: r.listed ? t('pages.registries.n_listed', { count: r.repositories.length }) : null,
        body: h('div', { class: 'stack' }, reg.kind === 'dockerhub' ? byNs : null, body, byName) }));
    } catch (e) {
      if (!signal.aborted) failed(repoSlot, e, listRepos);
    }
  };

  (async () => {
    mount(repoSlot, sk.tableRows(5));
    try {
      reg = await get(`/api/registries/${id}`, { signal });
    } catch (e) {
      if (!signal.aborted) mount(repoSlot, e.status === 404 ? emptyState(t('pages.registries.no_such'), t('pages.registries.no_such_text')) : errorState(e, () => location.reload()));
      return;
    }
    nsField.input.value = reg.username || '';
    mount(head, pageHeader(reg.name, reg.username ? t('pages.registries.at_as', { kind: kindLabel(reg.kind), url: reg.url, user: reg.username }) : t('pages.registries.at', { kind: kindLabel(reg.kind), url: reg.url }),
      button({ size: 'sm', href: '/registries' }, t('pages.registries.all'))));
    listRepos();
  })();
}
