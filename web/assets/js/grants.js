// Who has which role on an environment or a group ("Access" 1, 3): the whole list of people and teams, each with one
// role, saved at once. Administrators only.

import { h } from './dom.js';
import { get, put } from './api.js';
import { openModal } from '../finui/components/modal/modal.js';
import { button } from '../finui/components/button/button.js';
import { callout } from '../finui/components/callout/callout.js';
import { toast } from '../finui/components/toast/toast.js';
import { spinner } from '../finui/components/spinner/spinner.js';
import { t } from './i18n.js';

export const ROLES = [
  ['', t('grants.roles.none')],
  ['env_admin', t('grants.roles.env_admin')],
  ['operator', t('grants.roles.operator')],
  ['helpdesk', t('grants.roles.helpdesk')],
  ['standard', t('grants.roles.standard')],
  ['read_only', t('grants.roles.read_only')],
];
export const roleName = (r) => (r === 'admin' ? t('common.administrator') : (ROLES.find(([k]) => k === r) || [, r])[1]);

const HELP = t('grants.help');

/** The dialog for `/api/environments/{id}/access` or `/api/environment-groups/{id}/access`. */
export function grantsDialog({ title, path }) {
  const body = h('div', { class: 'stack' }, spinner(16));
  const m = openModal({ title, wide: true, body });
  Promise.all([get(path), get('/api/users'), get('/api/teams')]).then(([g, users, teams]) => {
    const pick = (kind, list, given) => list.map((x) => {
      const now = (given.find((y) => y.id === x.id) || {}).role || '';
      const sel = h('select', { class: 'fui-field__input', id: `grant-${kind}-${x.id}`, 'aria-label': t('grants.role_of', { name: x.name }) }, ROLES.map(([k, label]) => h('option', { value: k, selected: k === now }, label)));
      return { id: x.id, sel, row: h('div', { class: 'row' }, h('span', { class: 'grow truncate' }, x.name, x.role === 'admin' ? h('span', { class: 'faint' }, ' ' + t('grants.admin_everywhere')) : null), sel) };
    });
    const people = pick('user', users, g.users), groups = pick('team', teams, g.teams);
    const save = button({ variant: 'primary', onClick: async () => {
      const out = (rows) => rows.filter((r) => r.sel.value).map((r) => ({ id: r.id, role: r.sel.value }));
      save.disabled = true;
      try { await put(path, { users: out(people), teams: out(groups) }); toast({ text: t('common.saved'), tone: 'good' }); m.close(); }
      catch (e) { toast({ text: e.message, tone: 'critical' }); } finally { save.disabled = false; }
    } }, t('common.save'));
    body.replaceChildren(callout({ tone: 'info', body: HELP }),
      h('h3', { class: 'drawer-sub' }, t('grants.people')), ...(people.length ? people.map((p) => p.row) : [h('p', { class: 'muted' }, t('grants.nobody_yet'))]),
      h('h3', { class: 'drawer-sub' }, t('grants.teams')), ...(groups.length ? groups.map((p) => p.row) : [h('p', { class: 'muted' }, t('grants.no_teams'))]),
      h('div', { class: 'row row--end' }, save));
  }, (e) => body.replaceChildren(callout({ tone: 'critical', body: e.message })));
}
