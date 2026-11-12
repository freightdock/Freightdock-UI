// Environments: the Docker hosts Freightdock manages, their health, group and tags, and for administrators adding one (an
// agent that dials in, or a host on Docker's TLS port), editing, a new join token, and removing; under them the groups
// and the tags.

import { h, ago, when } from '../dom.js';
import { get, post, put, del } from '../api.js';
import { loadInto, table } from '../view.js';
import { state, isAdmin, setEnv, setEnvs } from '../state.js';
import { grantsDialog, roleName } from '../grants.js';
import { securityDialog } from '../security.js';
import { envBadge } from '../docker.js';
import { confirm } from '../confirm.js';
import { navigate } from '../router.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField, textArea, inlineError } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { callout } from '../../finui/components/callout/callout.js';
import { codeBlock, codeLine } from '../../finui/components/code-block/code-block.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { chip, chipSet } from '../../finui/components/chip/chip.js';
import { placementFields, placementLists, groupsCard, tagsCard } from './env-groups.js';
import { t } from '../i18n.js';

const KINDS = { local: t('pages.home.kind.local'), agent: t('pages.home.kind.agent'), tls: t('pages.home.kind.tls') };

/** The `docker run` line for an agent, for the address given. */
export const runLine = (image, address, token) =>
  `docker run -d --name freightdock-agent --restart unless-stopped \\\n  -v /var/run/docker.sock:/var/run/docker.sock \\\n  -v freightdock-agent:/data \\\n  -e FREIGHTDOCK_SERVER=${address} \\\n  -e FREIGHTDOCK_JOIN_TOKEN=${token} \\\n  ${image}`;

/** The join token, shown once: the line that starts the agent, with the address it dials open to change. */
export function showJoin(name, join) {
  const address = formField({ id: 'join-address', label: t('pages.environments.address_label'), autocomplete: 'off',
    help: t('pages.environments.address_help') });
  address.input.value = join.address || '';
  const slot = h('div');
  const paint = () => {
    const a = address.input.value.trim() || '<freightdock-address>:7444';
    slot.replaceChildren(codeBlock({ panes: [{ key: 'run', label: 'docker run', text: runLine(join.image, a, join.token) }], note: t('pages.environments.run_here') }));
  };
  address.input.addEventListener('input', paint);
  paint();
  openModal({ title: t('pages.environments.start_agent', { name }), wide: true, body: h('div', { class: 'stack' },
    callout({ tone: 'info', body: t('pages.environments.token_once', { when: when(join.expires_at) }) }),
    address.el, slot,
    h('div', { class: 'stack' }, h('span', { class: 'faint' }, t('pages.environments.token_alone')), codeLine(join.token, { label: t('pages.environments.copy_token') }))) });
}

/** FinUI's text area with a place for its error, as formField has. */
function area(opts) {
  const f = textArea(opts);
  const err = h('div');
  f.el.append(err);
  f.setError = (msg) => {
    f.input.setAttribute('aria-invalid', msg ? 'true' : 'false');
    err.replaceChildren(...(msg ? [inlineError(opts.id + '-err', msg)] : []));
  };
  return f;
}

function tlsFields(cfg = {}) {
  const url = formField({ id: 'env-url', label: t('pages.environments.address'), placeholder: 'tcp://docker.example:2376', autocomplete: 'off', help: t('pages.environments.tls_help') });
  url.input.value = cfg.url || '';
  const ca = area({ id: 'env-ca', label: t('pages.environments.ca'), rows: 4, placeholder: '-----BEGIN CERTIFICATE-----', help: t('pages.environments.ca_help') });
  const cert = area({ id: 'env-cert', label: t('pages.environments.cert'), rows: 4, placeholder: '-----BEGIN CERTIFICATE-----', help: t('pages.environments.cert_help') });
  const key = area({ id: 'env-key', label: t('pages.environments.key'), rows: 4, placeholder: cfg.has_cert ? t('pages.environments.key_kept') : '-----BEGIN PRIVATE KEY-----', help: t('pages.environments.key_help') }); // gitleaks:allow (a placeholder, no key)
  return { url, tls_ca: ca, tls_cert: cert, tls_key: key };
}

function addDialog(after) {
  let kind = 'agent';
  const name = formField({ id: 'env-name', label: t('common.name'), placeholder: 'vm-1', autocomplete: 'off' });
  const tls = tlsFields();
  const tlsBox = h('div', { class: 'stack', hidden: true }, tls.url.el, tls.tls_ca.el, tls.tls_cert.el, tls.tls_key.el);
  const about = h('p', { class: 'faint' });
  const say = () => {
    about.textContent = kind === 'agent'
      ? t('pages.environments.about_agent')
      : t('pages.environments.about_tls');
    tlsBox.hidden = kind !== 'tls';
  };
  const kinds = segmented({ label: t('pages.networks.kind'), value: kind, options: [{ value: 'agent', label: t('pages.home.kind.agent') }, { value: 'tls', label: t('pages.home.kind.tls') }], onChange: (v) => { kind = v; say(); } });
  say();
  const go = button({ variant: 'primary', type: 'submit' }, t('common.add'));
  const form = h('form', { class: 'stack', novalidate: true }, kinds, about, name.el, tlsBox, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.environments.add_title'), body: form, initialFocus: name.input });
  const fields = { name, ...tls };
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const f of Object.values(fields)) f.setError('');
    go.disabled = true;
    try {
      const body = { name: name.input.value, kind };
      if (kind === 'tls') for (const k of ['url', 'tls_ca', 'tls_cert', 'tls_key']) body[k] = fields[k].input.value;
      const r = await post('/api/environments', body);
      m.close();
      after();
      if (r.join) showJoin(r.environment.name, r.join); else toast({ text: t('pages.environments.added'), tone: 'good' });
    } catch (err) {
      const f = err.field && fields[err.field];
      if (f) { f.setError(err.message); f.input.focus(); } else name.setError(err.message);
    } finally { go.disabled = false; }
  });
}

/** Edits a host: its name, group and tags, and a TLS host's address and certificates. */
export async function editDialog(env, after) {
  let lists;
  try { lists = await placementLists(); } catch (e) { toast({ text: e.message, tone: 'critical' }); return; }
  const name = formField({ id: 'env-name', label: t('common.name'), autocomplete: 'off' });
  name.input.value = env.name;
  const place = placementFields(env, lists.groups, lists.tags);
  const tls = env.kind === 'tls' ? tlsFields({ url: env.url, has_cert: env.tls && env.tls.has_cert }) : null;
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, place.el,
    tls ? [tls.url.el, h('p', { class: 'faint' }, t('pages.environments.keep_certs')), tls.tls_ca.el, tls.tls_cert.el, tls.tls_key.el] : null,
    h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('routes.edit', { name: env.name }), body: form, initialFocus: name.input });
  const fields = { name, ...(tls || {}) };
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const f of Object.values(fields)) f.setError('');
    const body = { name: name.input.value, ...place.value() };
    if (tls) {
      if (tls.url.input.value.trim() !== env.url) body.url = tls.url.input.value;
      for (const k of ['tls_ca', 'tls_cert', 'tls_key']) if (tls[k].input.value.trim()) body[k] = tls[k].input.value;
    }
    try { await put(`/api/environments/${env.id}`, body); m.close(); toast({ text: t('common.saved'), tone: 'good' }); after(); } catch (err) {
      const f = err.field && fields[err.field];
      if (f) { f.setError(err.message); f.input.focus(); } else if (err.field === 'group_id' || err.field === 'tags') toast({ text: err.message, tone: 'critical' }); else name.setError(err.message);
    }
  });
}

async function newToken(env, after) {
  const ok = await confirm({ title: t('pages.environments.new_token_title', { name: env.name }), action: t('pages.environments.make_token'),
    body: env.agent && env.agent.enrolled ? t('pages.environments.new_token_enrolled') : t('pages.environments.new_token_body') });
  if (!ok) return;
  try { const join = await post(`/api/environments/${env.id}/join`); after(); showJoin(env.name, join); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

async function remove(env, after) {
  const ok = await confirm({ title: t('common.remove_name', { name: env.name }), body: env.kind === 'agent' ? t('pages.environments.remove_agent') : t('pages.environments.remove_body') });
  if (!ok) return;
  try { await del(`/api/environments/${env.id}`); toast({ text: t('common.removed'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** Where the host is, as people read it. */
function whereIs(e) {
  if (e.kind === 'agent') return e.agent && e.agent.peer ? e.agent.peer : '';
  return e.url;
}

function use(e) {
  setEnv(e.id);
  navigate('/');
}

export function environmentsPage({ el, signal, every }) {
  const slot = h('div');
  const lists = h('div');
  let envs = [];
  const listsLoad = loadInto(lists, {
    signal,
    skeleton: () => h('div'),
    load: (s) => placementLists(s),
    draw: (l, again) => h('div', { class: 'grid-2' }, groupsCard(l.groups, envs, () => { again(); reload(); }), tagsCard(l.tags, envs, () => { again(); reload(); })),
  });
  const reload = loadInto(slot, {
    signal,
    load: async (s) => { const list = await get('/api/environments', { signal: s }); setEnvs(list); envs = list; listsLoad.redraw(); return list; },
    draw: (list, again) => table({
      rows: list, empty: [t('pages.environments.empty'), t('pages.environments.empty_text')],
      columns: [
        { label: t('common.name'), cell: (e) => h('div', { class: 'name-cell' }, h('span', { class: 'truncate' }, e.name), e.id === state.env ? h('span', { class: 'sub' }, t('pages.environments.shown_now')) : null) },
        { label: t('pages.networks.kind'), cell: (e) => KINDS[e.kind] || e.kind },
        { label: t('pages.environments.your_role'), cell: (e) => roleName(e.role) },
        { label: t('pages.home.group'), cell: (e) => (e.group ? e.group.name : h('span', { class: 'faint' }, t('common.none_word'))), sort: (e) => (e.group ? e.group.name : '') },
        { label: t('pages.images.tags'), nosort: true, cell: (e) => (e.tags.length ? chipSet(...e.tags.map((tag) => chip(tag))) : null) },
        { label: t('pages.home.health'), cell: (e) => h('span', { title: e.last_error || '' }, envBadge(e)), sort: (e) => e.status },
        { label: 'Docker', cell: (e) => e.docker_version || h('span', { class: 'faint' }, ''), cls: 'mono' },
        { label: t('pages.environments.where'), cell: (e) => h('span', { class: 'mono truncate' }, whereIs(e)) },
        { label: t('pages.environments.last_seen'), cell: (e) => (e.last_seen_at ? h('span', { class: 'nowrap', title: when(e.last_seen_at) }, ago(e.last_seen_at)) : h('span', { class: 'faint' }, t('common.never'))), sort: (e) => e.last_seen_at || 0 },
        { label: '', nosort: true, cell: (e) => h('div', { class: 'cell-actions' },
          e.id !== state.env ? button({ size: 'sm', onClick: () => use(e) }, t('common.show')) : null,
          isAdmin() ? button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.environments.who_has_role', { name: e.name }), title: t('ownership.access'), onClick: () => grantsDialog({ title: t('pages.environments.access_to', { name: e.name }), path: `/api/environments/${e.id}/access` }) }, icon('users', 14)) : null,
          isAdmin() ? button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.environments.security_of', { name: e.name }), title: t('pages.environments.security_settings'), onClick: () => securityDialog({ title: t('pages.environments.security_on', { name: e.name }), path: `/api/environments/${e.id}/security` }) }, icon('shield', 14)) : null,
          isAdmin() ? button({ variant: 'icon', size: 'sm', 'aria-label': t('routes.edit', { name: e.name }), onClick: () => editDialog(e, () => { again(); listsLoad(); }) }, icon('settings', 14)) : null,
          isAdmin() && e.kind === 'agent' ? button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.environments.new_token_for', { name: e.name }), title: t('pages.environments.new_token'), onClick: () => newToken(e, again) }, icon('lock', 14)) : null,
          isAdmin() && e.kind !== 'local' ? button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: e.name }), onClick: () => remove(e, again) }, icon('trash', 14)) : null) },
      ],
    }),
  });
  every(10000, reload);
  const actions = isAdmin() ? button({ size: 'sm', variant: 'primary', onClick: () => addDialog(reload) }, icon('plus', 14), t('pages.environments.add')) : null;
  el.append(pageHeader(t('nav.environments'), t('pages.environments.sub'), actions), slot, lists);
}
