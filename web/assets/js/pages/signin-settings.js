// Settings' sign-in part, for administrators: the sign-in providers (OpenID Connect, with presets for Google and
// Microsoft Entra ID; GitHub; LDAP and Active Directory), each its own button or a directory behind the password form,
// with who may come in, as what, and into which teams; and whether every sign-in with a password needs a second factor.
// The local sign-in always stays ("SSO" 4). Secrets are typed in and never shown again ("Secrets" 1).

import { h, icon } from '../dom.js';
import { get, put, post, del } from '../api.js';
import { loadInto } from '../view.js';
import { confirm } from '../confirm.js';
import { card } from '../../finui/components/card/card.js';
import { formField, textArea, inlineError } from '../../finui/components/field/field.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { button } from '../../finui/components/button/button.js';
import { callout } from '../../finui/components/callout/callout.js';
import { codeLine } from '../../finui/components/code-block/code-block.js';
import { toast } from '../../finui/components/toast/toast.js';
import { spinner } from '../../finui/components/spinner/spinner.js';
import { facts } from '../../finui/components/facts/facts.js';
import { status } from '../../finui/components/badge/badge.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { t } from '../i18n.js';

const list = (s) => s.split(/[,\n]+/).map((x) => x.trim()).filter(Boolean);
const callbackFor = (id) => `${location.origin}/api/auth/sso/${id || '<id>'}/callback`;

const PRESET_NAMES = { '': 'OpenID Connect', google: 'Google', microsoft: 'Microsoft Entra ID', github: 'GitHub', ldap: 'LDAP', ad: 'Active Directory' };
const KIND_NAMES = { oidc: 'OpenID Connect', github: 'GitHub', ldap: t('pages.users.directory') };
const presetName = (p) => PRESET_NAMES[p.preset || ''] || KIND_NAMES[p.kind];

/** What to tell an administrator setting a preset up at the provider. */
function howto(p, id) {
  const cb = () => codeLine(callbackFor(id), { label: t('pages.signin_settings.copy_redirect') });
  switch (p.preset) {
    case 'google': return callout({ tone: 'info', title: t('pages.signin_settings.in_google'), body: h('div', { class: 'stack' },
      h('p', null, t('pages.signin_settings.google_help')), cb()) });
    case 'microsoft': return callout({ tone: 'info', title: t('pages.signin_settings.in_entra'), body: h('div', { class: 'stack' },
      h('p', null, t('pages.signin_settings.entra_help')), cb()) });
    case 'github': return callout({ tone: 'info', title: t('pages.signin_settings.on_github'), body: h('div', { class: 'stack' },
      h('p', null, t('pages.signin_settings.github_help')), cb()) });
    case 'ldap': case 'ad': return callout({ tone: 'info', body: t('pages.signin_settings.ldap_help') });
    default: return callout({ tone: 'info', title: t('pages.signin_settings.in_authentik'), body: h('div', { class: 'stack' },
      h('p', null, t('pages.signin_settings.authentik_help')), cb()) });
  }
}

/** The editor for one provider, new (from a preset) or kept. */
function editor(start, isNew, after) {
  const p = structuredClone(start);
  p.ldap = p.ldap || {};
  p.teams = p.teams || { enabled: false, create_missing: false, rules: [] };
  const f = {};
  const field = (key, label, value, opts = {}) => {
    f[key] = formField({ id: `p-${key}`, label, ...opts });
    f[key].input.value = value == null ? '' : value;
    return f[key].el;
  };
  const area = (key, label, value, opts = {}) => {
    const box = textArea({ id: `p-${key}`, label, ...opts });
    box.input.value = value || '';
    const err = h('div');
    f[key] = { el: h('div', null, box.el, err), input: box.input, setError: (m) => { err.replaceChildren(m ? inlineError(`p-${key}-err`, m) : ''); } };
    return f[key].el;
  };
  const sw = (key, label, help, value, set) => settingRow({ id: `p-${key}`, label, help, control: toggle({ checked: !!value, labelledby: `p-${key}-label`, onChange: set }) });
  const ldap = p.kind === 'ldap', github = p.kind === 'github';

  const idField = isNew ? field('id', t('pages.signin_settings.id'), p.id, { help: t('pages.signin_settings.id_help'), autocomplete: 'off' }) : null;
  const sections = [
    sw('enabled', t('pages.account.on'), ldap ? t('pages.signin_settings.on_ldap') : t('pages.signin_settings.on_oidc'), p.enabled, (v) => { p.enabled = v; }),
    idField,
    field('label', t('common.name'), p.label, { help: ldap ? t('pages.signin_settings.label_ldap') : t('pages.signin_settings.label_oidc') }),
  ];
  const result = h('div');
  if (!ldap) {
    const test = github ? null : button({ size: 'sm', onClick: async () => {
      f.issuer.setError(''); result.replaceChildren(spinner(14));
      try { const r = await post('/api/settings/sso/test', { issuer: f.issuer.input.value }); result.replaceChildren(callout({ tone: 'good', body: t('pages.signin_settings.answers_as', { issuer: r.issuer }) })); } catch (e) { result.replaceChildren(); f.issuer.setError(e.message); }
    } }, t('pages.signin_settings.test_issuer'));
    sections.push(
      h('div', { class: 'stack' }, field('issuer', github ? t('pages.signin_settings.github_address') : t('pages.signin_settings.issuer'), p.issuer, { help: github ? t('pages.signin_settings.github_address_help') : t('pages.signin_settings.issuer_help') }), test ? h('div', { class: 'row' }, test) : null, result),
      github ? field('api_url', t('pages.signin_settings.github_api'), p.api_url, { help: t('pages.signin_settings.github_api_help') }) : null,
      field('client_id', t('pages.signin_settings.client_id'), p.client_id, { autocomplete: 'off' }),
      field('client_secret', t('pages.signin_settings.client_secret'), '', { type: 'password', autocomplete: 'new-password', placeholder: p.has_secret ? t('pages.registries.kept') : '', help: t('pages.signin_settings.sealed') }),
      field('redirect_uri', t('pages.signin_settings.redirect_uri'), p.redirect_uri || callbackFor(p.id), { help: t('pages.signin_settings.redirect_help') }),
      field('scopes', t('pages.signin_settings.scopes'), p.scopes),
      github ? null : field('username_claim', t('pages.signin_settings.name_claim'), p.username_claim, { help: t('pages.signin_settings.name_claim_help') }),
      github ? null : field('groups_claim', t('pages.signin_settings.groups_claim'), p.groups_claim));
  } else {
    const l = p.ldap;
    let mode = l.group_mode || 'search';
    const groupSearch = h('div', { class: 'stack' }, field('group_base', t('pages.signin_settings.group_base'), l.group_base), field('group_filter', t('pages.signin_settings.group_filter'), l.group_filter, { help: t('pages.signin_settings.group_filter_help') }), field('group_name_attribute', t('pages.signin_settings.group_name_attr'), l.group_name_attribute));
    groupSearch.hidden = mode !== 'search';
    sections.push(
      field('url', t('pages.environments.address'), l.url, { placeholder: 'ldaps://ldap.example.org', help: t('pages.signin_settings.ldap_url_help') }),
      sw('starttls', 'StartTLS', t('pages.signin_settings.starttls_help'), l.starttls, (v) => { l.starttls = v; }),
      sw('allow_plain', t('pages.signin_settings.allow_plain'), t('pages.signin_settings.allow_plain_help'), l.allow_plain, (v) => { l.allow_plain = v; }),
      area('ca_pem', t('pages.environments.ca'), l.ca_pem, { rows: 3, help: t('pages.signin_settings.ca_help'), placeholder: '-----BEGIN CERTIFICATE-----' }),
      field('bind_dn', t('pages.signin_settings.bind_dn'), l.bind_dn, { help: t('pages.signin_settings.bind_dn_help'), autocomplete: 'off' }),
      field('bind_password', t('pages.signin_settings.its_password'), '', { type: 'password', autocomplete: 'new-password', placeholder: l.has_bind_password ? t('pages.registries.kept') : '', help: t('pages.signin_settings.bind_password_help') }),
      field('user_base', t('pages.signin_settings.user_base'), l.user_base),
      field('user_filter', t('pages.signin_settings.user_filter'), l.user_filter, { help: t('pages.signin_settings.user_filter_help') }),
      field('username_attribute', t('pages.signin_settings.name_attr'), l.username_attribute),
      field('email_attribute', t('pages.signin_settings.email_attr'), l.email_attribute),
      field('id_attribute', t('pages.signin_settings.id_attr'), l.id_attribute, { help: t('pages.signin_settings.id_attr_help') }),
      h('div', { class: 'stack' }, h('span', { class: 'faint' }, t('pages.signin_settings.groups_from')), segmented({ label: t('pages.signin_settings.groups_from'), value: mode, options: [{ value: 'search', label: t('pages.signin_settings.group_search') }, { value: 'member_of', label: t('pages.signin_settings.member_of') }], onChange: (v) => { mode = v; l.group_mode = v; groupSearch.hidden = v !== 'search'; } })),
      groupSearch);
  }
  sections.push(
    field('admin_groups', t('pages.signin_settings.admin_groups'), (p.admin_groups || []).join(', '), { help: github ? t('pages.signin_settings.admin_groups_github') : t('pages.signin_settings.admin_groups_help') }),
    field('user_groups', t('pages.signin_settings.user_groups'), (p.user_groups || []).join(', '), { help: t('pages.signin_settings.user_groups_help') }),
    ldap ? null : field('allowed_domains', t('pages.signin_settings.domains'), (p.allowed_domains || []).join(', '), { help: t('pages.signin_settings.domains_help') }),
    sw('link', t('pages.signin_settings.link'), t('pages.signin_settings.link_help'), p.link_by_name !== false, (v) => { p.link_by_name = v; }),
    sw('teams-on', t('pages.signin_settings.teams_on'), t('pages.signin_settings.teams_on_help'), p.teams.enabled, (v) => { p.teams.enabled = v; }),
    sw('teams-make', t('pages.signin_settings.teams_make'), t('pages.signin_settings.teams_make_help'), p.teams.create_missing, (v) => { p.teams.create_missing = v; }),
    area('teams', t('pages.signin_settings.team_rules'), (p.teams.rules || []).map((r) => `${r.group} -> ${r.team}`).join('\n'), { rows: 3, placeholder: 'devs -> Developers\n/^team-(.+)$/ -> Team $1', help: t('pages.signin_settings.team_rules_help') }));

  // A test of the directory: the connection and the service account, and a person's groups, role and teams.
  const tested = h('div');
  const testName = formField({ id: 'p-test-name', label: t('pages.signin_settings.test_user'), autocomplete: 'off', help: t('pages.signin_settings.test_user_help') });
  const testPassword = formField({ id: 'p-test-password', label: t('pages.signin_settings.their_password'), type: 'password', autocomplete: 'off' });
  const runTest = (withUser) => async () => {
    tested.replaceChildren(spinner(14));
    try {
      const r = await post(`/api/settings/sso/${encodeURIComponent(p.id || f.id?.input.value || 'new')}/ldap-test`, { provider: body(), name: withUser ? testName.input.value : '', password: withUser ? testPassword.input.value : '' });
      const lines = [[t('pages.signin_settings.connected'), r.tls ? t('pages.signin_settings.yes_tls') : t('pages.signin_settings.yes_plain')]];
      if (withUser) {
        const u = r.user;
        if (!u) lines.push([t('nav.user'), t('pages.signin_settings.not_found')]);
        else lines.push(['DN', u.dn, { wide: true, mono: true }], [t('common.name'), u.name], [t('pages.signin_settings.email'), u.email || ''], [t('pages.env_groups.groups'), (u.groups || []).filter((g) => !g.includes('=')).join(', ') || t('common.none_word'), { wide: true }],
          [t('pages.signin_settings.would_be'), u.role === 'admin' ? t('common.administrator') : u.role === 'user' ? t('nav.user') : t('pages.signin_settings.turned_away')], [t('grants.teams'), (u.teams || []).join(', ') || t('common.none_word')],
          [t('auth.password'), u.password_ok == null ? t('docker.not_checked') : u.password_ok ? t('pages.signin_settings.right') : t('pages.signin_settings.wrong')]);
      }
      tested.replaceChildren(facts(lines));
    } catch (e) { tested.replaceChildren(callout({ tone: 'critical', body: e.message })); }
  };
  const testing = ldap ? h('div', { class: 'stack' },
    h('div', { class: 'row' }, button({ size: 'sm', onClick: runTest(false) }, t('pages.signin_settings.test_connection'))),
    testName.el, testPassword.el, h('div', { class: 'row' }, button({ size: 'sm', onClick: runTest(true) }, t('pages.signin_settings.test_the_user'))), tested) : null;

  function body() {
    const v = (k) => (f[k] ? f[k].input.value : undefined);
    const out = { ...p, label: v('label'), admin_groups: list(v('admin_groups')), user_groups: list(v('user_groups')), allowed_domains: ldap ? [] : list(v('allowed_domains')) };
    out.teams = { ...p.teams, rules: v('teams').split('\n').map((x) => x.trim()).filter(Boolean).map((line) => {
      const i = line.lastIndexOf('->');
      return i < 0 ? { group: line.trim(), team: '' } : { group: line.slice(0, i).trim(), team: line.slice(i + 2).trim() };
    }) };
    if (ldap) {
      out.ldap = { ...p.ldap };
      for (const k of ['url', 'ca_pem', 'bind_dn', 'bind_password', 'user_base', 'user_filter', 'username_attribute', 'email_attribute', 'id_attribute', 'group_base', 'group_filter', 'group_name_attribute']) out.ldap[k] = v(k);
    } else {
      for (const k of ['issuer', 'api_url', 'client_id', 'client_secret', 'redirect_uri', 'scopes', 'username_claim', 'groups_claim']) if (f[k]) out[k] = v(k);
    }
    delete out.has_secret;
    delete out.ldap.has_bind_password;
    return out;
  }

  // A new provider's redirect address follows its id as it is typed.
  if (isNew && f.id && f.redirect_uri) f.id.input.addEventListener('input', () => { f.redirect_uri.input.value = callbackFor(f.id.input.value); });

  const save = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, howto(p, p.id), ...sections, testing, h('div', { class: 'row row--end' }, save));
  const m = openModal({ title: isNew ? t('pages.signin_settings.add_title', { name: presetName(p) }) : p.label, wide: true, body: form, initialFocus: (f.id || f.label).input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const x of Object.values(f)) x.setError('');
    const id = isNew ? f.id.input.value.trim() : p.id;
    save.disabled = true;
    try {
      await put(`/api/settings/sso/${encodeURIComponent(id)}`, body());
      m.close();
      toast({ text: p.enabled ? (f.label.input.value ? t('pages.signin_settings.is_on', { name: f.label.input.value }) : t('pages.signin_settings.provider_on')) : t('common.saved'), tone: 'good' });
      after();
    } catch (err) {
      const key = { rules: 'teams', starttls: 'url' }[err.field] || err.field;
      if (key && f[key]) { f[key].setError(err.message); f[key].input.focus(); } else toast({ text: err.message, tone: 'critical' });
    } finally { save.disabled = false; }
  });
}

async function remove(p, after) {
  if (!(await confirm({ title: t('common.remove_name', { name: p.label }), action: t('common.remove'), body: t('pages.signin_settings.remove_body') }))) return;
  try { await del(`/api/settings/sso/${encodeURIComponent(p.id)}`); toast({ text: t('common.removed'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** A new provider's id: the preset's name, or with a number when that is taken. */
function freshId(preset, taken) {
  const base = preset.preset || (preset.kind === 'oidc' ? 'sso' : preset.kind);
  let id = base;
  for (let i = 2; taken.includes(id); i++) id = `${base}-${i}`;
  return id;
}

/** Every provider, each with its kind and whether it is on, and a menu to add one from a preset. */
export function providersCard(cfg, again) {
  const ids = cfg.providers.map((p) => p.id);
  const add = dropdownMenu({ label: t('pages.signin_settings.add_provider'), icon: 'plus', size: 'sm', items: cfg.presets.map((pr) => ({
    label: presetName(pr), onSelect: () => { const id = freshId(pr, ids); editor({ ...pr, id, redirect_uri: pr.kind === 'ldap' ? '' : callbackFor(id) }, true, again); },
  })) });
  const rows = cfg.providers.length
    ? h('div', { class: 'stack' }, ...cfg.providers.map((p) => h('div', { class: 'fui-setting-row' },
      h('div', null, h('div', { class: 'fui-setting-row__label' }, p.label), h('p', { class: 'fui-field__help' }, `${presetName(p)} · ${p.id}`)),
      h('div', { class: 'fui-setting-row__control row' },
        p.enabled ? status({ tone: 'good', line: true }, t('pages.account.on')) : status({ tone: 'info', line: true }, t('pages.signin_settings.off')),
        p.kind === 'ldap' && p.enabled && p.ldap && p.ldap.allow_plain && !p.ldap.starttls && String(p.ldap.url).startsWith('ldap://') ? status({ tone: 'warning' }, t('pages.signin_settings.plain_ldap')) : null,
        button({ size: 'sm', onClick: () => editor(p, false, again) }, icon('pencil', 14), t('common.edit')),
        button({ size: 'sm', variant: 'icon', 'aria-label': t('common.remove_label', { name: p.label }), onClick: () => remove(p, again) }, icon('trash', 14))))))
    : h('p', { class: 'faint' }, t('pages.signin_settings.none_yet'));
  return card({ title: t('pages.signin_settings.providers'), sub: t('pages.signin_settings.providers_sub'), actions: add, body: rows });
}

/** Whether every sign-in with a password needs a second factor. */
export function twoFactorCard(cfg) {
  let required = !!cfg.required;
  const note = h('div');
  const paint = () => note.replaceChildren(required && cfg.local_without ? callout({ tone: 'info', body: t('pages.signin_settings.without', { count: cfg.local_without }) }) : '');
  paint();
  const sw = toggle({ checked: required, labelledby: 'tf-required-label', onChange: async (v) => {
    try { await put('/api/settings/two-factor', { required: v }); required = v; paint(); toast({ text: v ? t('pages.signin_settings.tf_required') : t('pages.signin_settings.tf_optional'), tone: 'good' }); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
  } });
  return card({ title: t('pages.account.two_factor'), sub: t('pages.signin_settings.tf_sub'), body: h('div', { class: 'stack' },
    h('div', { class: 'fui-setting-row__rows' }, settingRow({ id: 'tf-required', label: t('pages.signin_settings.required_all'), help: t('pages.signin_settings.required_all_help'), control: sw })),
    note) });
}

/** Both, loaded into `providers` and `tf`. */
export function signinSettings(providers, tf, signal) {
  loadInto(providers, { signal, load: (s) => get('/api/settings/sso', { signal: s }), draw: providersCard });
  loadInto(tf, { signal, load: (s) => get('/api/settings/two-factor', { signal: s }), draw: twoFactorCard });
}
