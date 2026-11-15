// Who besides an administrator may operate a stack or a node ("Access" 24): the grants on it, each to a person or a
// team, given and taken away here by an administrator. Operating a stack is starting, stopping and restarting it; a
// node, reading its Docker pages and starting, stopping and restarting its containers.

import { h, mount } from './dom.js';
import { get, post, del } from './api.js';
import { openModal } from '../finui/components/modal/modal.js';
import { button } from '../finui/components/button/button.js';
import { toast } from '../finui/components/toast/toast.js';
import { t } from './i18n.js';

/** The dialog for the grants on `kind` ('stack' or 'node') `target`, shown as `label`. */
export function grantsDialog(kind, target, label) {
  const list = h('div', { class: 'stack' });
  const who = h('select', { class: 'fui-field__input', id: 'grant-who', 'aria-label': t('operators.who') });
  const said = h('p', { class: 'sub', 'aria-live': 'polite' });
  const add = button({ variant: 'primary', type: 'submit' }, t('operators.add'));
  const form = h('form', { class: 'row row--bottom', novalidate: true }, h('div', { class: 'fui-field grow' }, h('label', { htmlFor: 'grant-who', class: 'fui-field__label' }, t('operators.who')), who), add);
  const body = h('div', { class: 'stack' }, h('p', null, t(`operators.what.${kind}`)), list, form, said);
  openModal({ title: t('operators.title', { name: label }), body, initialFocus: who });

  async function load() {
    try {
      const [g, users, teams] = await Promise.all([get(`/api/cluster/grants?kind=${kind}&target=${encodeURIComponent(target)}`), get('/api/users'), get('/api/teams')]);
      const rows = g.grants || [];
      mount(list, rows.length
        ? rows.map((x) => h('div', { class: 'row' },
          h('span', { class: 'grow' }, x.team ? t('operators.team', { name: x.team.name || '' }) : (x.user && x.user.name) || ''),
          button({ size: 'sm', variant: 'danger', onClick: () => remove(x.id) }, t('common.remove'))))
        : h('p', { class: 'sub' }, t('operators.none')));
      const people = (users.users || users || []).filter((u) => u.role !== 'admin' && !u.disabled);
      const groups = teams.teams || teams || [];
      mount(who,
        h('option', { value: '' }, t('operators.choose')),
        people.map((u) => h('option', { value: `user:${u.id}` }, u.name)),
        groups.map((x) => h('option', { value: `team:${x.id}` }, t('operators.team', { name: x.name }))));
    } catch (e) { said.textContent = e.message; }
  }

  async function remove(id) {
    try { await del(`/api/cluster/grants/${id}`); load(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
  }

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const [k, id] = who.value.split(':');
    if (!id) { said.textContent = t('operators.choose'); return; }
    add.disabled = true;
    said.textContent = '';
    try {
      await post('/api/cluster/grants', { kind, target, [k === 'team' ? 'team_id' : 'user_id']: Number(id) });
      load();
    } catch (x) { said.textContent = x.message; } finally { add.disabled = false; }
  });
  load();
}
