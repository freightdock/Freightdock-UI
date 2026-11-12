// Users and teams, for administrators ("Access" 8): local users made with a password, their role, disabled or not,
// a new password, renamed, removed, their two-factor reset; whether they sign in here, through SSO or a directory, and
// when they last did; teams and who is
// in them. Roles on environments are given from Environments, per host or per group.

import { h, ago, when } from '../dom.js';
import { get, post, put, del } from '../api.js';
import { state } from '../state.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

/** A form in a modal: its fields, one submit, the error the server names put on its field. */
function formDialog({ title, fields, submit, extra = null, go: label = t('common.save') }) {
  const go = button({ variant: 'primary', type: 'submit' }, label);
  const form = h('form', { class: 'stack', novalidate: true }, ...Object.values(fields).map((f) => f.el), extra, h('div', { class: 'row row--end' }, go));
  const first = Object.values(fields)[0];
  const m = openModal({ title, body: form, initialFocus: first ? first.input : undefined });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const f of Object.values(fields)) f.setError('');
    go.disabled = true;
    try { await submit(); m.close(); }
    catch (err) { const f = fields[err.field] || first; if (f) { f.setError(err.message); f.input.focus(); } else toast({ text: err.message, tone: 'critical' }); }
    finally { go.disabled = false; }
  });
}

function userDialog(u, after) {
  const fields = { name: formField({ id: 'u-name', label: t('common.name'), autocomplete: 'off' }) };
  fields.name.input.value = u ? u.name : '';
  if (!u) fields.password = formField({ id: 'u-password', label: t('auth.password'), type: 'password', autocomplete: 'new-password', help: t('pages.users.password_help') });
  let admin = u ? u.role === 'admin' : false;
  const self = u && state.user && u.id === state.user.id;
  const sw = toggle({ checked: admin, labelledby: 'u-admin-label', disabled: self, onChange: (v) => { admin = v; } });
  const extra = h('div', { class: 'row' }, sw, h('div', { class: 'stack' }, h('span', { id: 'u-admin-label' }, t('common.administrator')),
    h('span', { class: 'faint' }, self ? t('pages.users.not_yourself') : t('pages.users.admin_help'))));
  formDialog({ title: u ? t('routes.edit', { name: u.name }) : t('pages.users.new_user_title'), fields, extra, go: u ? t('common.save') : t('pages.users.make_user'), submit: async () => {
    const role = admin ? 'admin' : 'user';
    if (u) await put(`/api/users/${u.id}`, { name: fields.name.input.value, ...(self ? {} : { role }) });
    else await post('/api/users', { name: fields.name.input.value, password: fields.password.input.value, role });
    toast({ text: t('common.saved'), tone: 'good' });
    after();
  } });
}

function passwordDialog(u, after) {
  const fields = { password: formField({ id: 'u-new-password', label: t('pages.users.new_password'), type: 'password', autocomplete: 'new-password', help: t('pages.users.new_password_help') }) };
  formDialog({ title: t('pages.users.new_password_for', { name: u.name }), fields, submit: async () => { await post(`/api/users/${u.id}/password`, { password: fields.password.input.value }); toast({ text: t('pages.users.changed'), tone: 'good' }); after(); } });
}

async function setDisabled(u, disabled, after) {
  if (disabled && !(await confirm({ title: t('pages.users.disable_title', { name: u.name }), action: t('pages.users.disable'), body: t('pages.users.disable_body') }))) return;
  try { await put(`/api/users/${u.id}`, { disabled }); toast({ text: disabled ? t('pages.users.disabled_done') : t('pages.users.enabled_done'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

/** A lost phone: the second factor taken away, so they sign in with the password (and enrol again, if it is required). */
async function resetTotp(u, after) {
  if (!(await confirm({ title: t('pages.users.reset_totp_title', { name: u.name }), action: t('pages.users.reset'), body: t('pages.users.reset_totp_body') }))) return;
  try { await del(`/api/users/${u.id}/totp`); toast({ text: t('pages.users.totp_reset'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

async function removeUser(u, after) {
  if (!(await confirm({ title: t('common.remove_name', { name: u.name }), body: t('pages.users.remove_body') }))) return;
  try { await del(`/api/users/${u.id}`); toast({ text: t('common.removed'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

function usersCard(signal) {
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get('/api/users', { signal: s }),
    draw: (list, again) => table({
      rows: list, label: t('pages.users.filter'), empty: [t('pages.users.nobody'), ''], rowClass: (u) => (u.disabled ? 'is-quiet' : null),
      columns: [
        { label: t('common.name'), cell: (u) => h('div', { class: 'name-cell' }, h('span', { class: 'truncate' }, u.name), u.email ? h('span', { class: 'sub' }, u.email) : null) },
        { label: t('pages.swarm.role'), cell: (u) => (u.role === 'admin' ? status({ tone: 'warning' }, t('common.administrator')) : h('span', null, t('pages.users.reads_the_cluster'))), sort: (u) => u.role },
        { label: t('pages.stack_git.signs_in'), cell: (u) => h('div', { class: 'row' },
          u.kind === 'sso' ? status({ tone: 'info', line: true }, 'SSO') : u.kind === 'ldap' ? status({ tone: 'info', line: true }, t('pages.users.directory')) : status({ tone: 'good', line: true, icon: 'key' }, t('auth.password')),
          u.totp ? status({ tone: 'good', line: true, icon: 'lock' }, t('pages.users.two_factor')) : null), sort: (u) => u.kind },
        { label: t('grants.teams'), cell: (u) => h('span', { class: 'truncate' }, (u.teams || []).map((team) => team.name).join(', ')) },
        { label: t('pages.users.last_sign_in'), cell: (u) => (u.last_login_at ? h('span', { class: 'nowrap', title: when(u.last_login_at) }, ago(u.last_login_at)) : h('span', { class: 'faint' }, t('common.never'))), sort: (u) => u.last_login_at || 0 },
        { label: t('common.state'), cell: (u) => (u.disabled ? status({ tone: 'critical' }, t('pages.users.disabled')) : status({ tone: 'good', line: true }, t('pages.users.active'))), sort: (u) => u.disabled },
        { label: '', nosort: true, cell: (u) => {
          const self = state.user && u.id === state.user.id;
          return h('div', { class: 'cell-actions' }, dropdownMenu({ label: t('common.actions_for', { name: u.name }), icon: 'moreHorizontal', iconOnly: true, variant: 'ghost', size: 'sm', items: [
            { label: t('common.edit'), icon: 'pencil', onSelect: () => userDialog(u, again) },
            u.kind === 'local' ? { label: t('pages.users.new_password'), icon: 'key', onSelect: () => passwordDialog(u, again) } : null,
            u.totp ? { label: t('pages.users.reset_totp'), icon: 'lock', onSelect: () => resetTotp(u, again) } : null,
            self ? null : u.disabled ? { label: t('pages.users.enable'), icon: 'play', onSelect: () => setDisabled(u, false, again) } : { label: t('pages.users.disable'), icon: 'pause', onSelect: () => setDisabled(u, true, again) },
            self ? null : { separator: true },
            self ? null : { label: t('common.remove'), icon: 'trash', danger: true, onSelect: () => removeUser(u, again) },
          ] }));
        } },
      ],
    }),
  });
  return { reload, el: card({ title: t('nav.users'), sub: t('pages.users.users_sub'), actions: button({ size: 'sm', variant: 'primary', onClick: () => userDialog(null, reload) }, icon('plus', 14), t('pages.users.new_user')), body: slot }) };
}

function teamDialog(team, users, after) {
  const fields = { name: formField({ id: 't-name', label: t('common.name'), autocomplete: 'off' }) };
  fields.name.input.value = team ? team.name : '';
  const now = new Set((team ? team.members : []).map((m) => m.id));
  const boxes = users.map((u) => ({ id: u.id, box: h('input', { type: 'checkbox', id: `t-m-${u.id}`, checked: now.has(u.id) }), label: u.name }));
  const extra = h('fieldset', { class: 'stack' }, h('legend', { class: 'fui-field__label' }, t('pages.users.members')),
    ...boxes.map((b) => h('label', { class: 'row', for: `t-m-${b.id}` }, b.box, h('span', null, b.label))));
  formDialog({ title: team ? t('routes.edit', { name: team.name }) : t('pages.users.new_team_title'), fields, extra, submit: async () => {
    const body = { name: fields.name.input.value, members: boxes.filter((b) => b.box.checked).map((b) => b.id) };
    if (team) await put(`/api/teams/${team.id}`, body); else await post('/api/teams', body);
    toast({ text: t('common.saved'), tone: 'good' });
    after();
  } });
}

function teamsCard(signal) {
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => Promise.all([get('/api/teams', { signal: s }), get('/api/users', { signal: s })]),
    draw: ([list, people], again) => table({
      rows: list, filter: false, empty: [t('pages.users.no_teams'), t('pages.users.no_teams_text')],
      columns: [
        { label: t('common.name'), cell: (team) => team.name },
        { label: t('pages.users.members'), cell: (team) => h('span', { class: 'truncate' }, (team.members || []).map((m) => m.name).join(', ') || t('pages.registries.nobody')), sort: (team) => (team.members || []).length },
        { label: '', nosort: true, cell: (team) => h('div', { class: 'cell-actions' },
          button({ variant: 'icon', size: 'sm', 'aria-label': t('routes.edit', { name: team.name }), onClick: () => teamDialog(team, people, again) }, icon('pencil', 14)),
          button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: team.name }), onClick: async () => {
            if (!(await confirm({ title: t('common.remove_name', { name: team.name }), body: t('pages.users.remove_team_body') }))) return;
            try { await del(`/api/teams/${team.id}`); toast({ text: t('common.removed'), tone: 'good' }); again(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
          } }, icon('trash', 14))) },
      ],
    }),
  });
  const make = button({ size: 'sm', variant: 'primary', onClick: async () => teamDialog(null, await get('/api/users'), reload) }, icon('plus', 14), t('pages.users.new_team'));
  return card({ title: t('grants.teams'), sub: t('pages.users.teams_sub'), actions: make, body: slot });
}

export function usersPage({ el, signal }) {
  const users = usersCard(signal);
  const teams = teamsCard(signal);
  el.append(pageHeader(t('nav.users'), t('pages.users.sub')), h('div', { class: 'stack' }, users.el, teams));
}
