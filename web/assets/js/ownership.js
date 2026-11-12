// Who a container, a stack, a volume, a network or an image belongs to ("Access" 4, 18): public, a team's, private to
// one person, or the administrators'. A card for its page, and a picker for the forms that make one.

import { h } from './dom.js';
import { get, put } from './api.js';
import { isAdmin, can, currentEnv } from './state.js';
import { card } from '../finui/components/card/card.js';
import { facts } from '../finui/components/facts/facts.js';
import { button } from '../finui/components/button/button.js';
import { toast } from '../finui/components/toast/toast.js';
import { t } from './i18n.js';

export const LEVELS = { public: t('ownership.levels.public'), team: t('ownership.levels.team'), private: t('ownership.levels.private'), admins: t('ownership.levels.admins') };

/** Whether the person sees everything on the host shown (an Administrator, an Environment administrator,
 *  an Operator, Helpdesk), and so may keep a thing to the administrators or give it to anybody. */
const seesAll = () => isAdmin() || ['env_admin', 'operator', 'helpdesk'].includes((currentEnv() || {}).role);

/** How an ownership reads: "Public", "Team ops", "Private to ola", "Administrators only". */
export function describe(a) {
  if (!a || a.level === 'admins') return LEVELS.admins;
  if (a.level === 'public') return t('ownership.public');
  if (a.level === 'team') return t('ownership.team', { name: (a.team && a.team.name) || '' }).trim();
  return a.user && a.user.name ? t('ownership.private_to', { name: a.user.name }) : t('ownership.private_to_nobody');
}

/** The teams one may share with: every team for those who see everything, else one's own (the server says which). */
const teams = () => get('/api/teams').catch(() => []);

/** A picker: which level, and which team when a team. `value()` is what the API takes as `access`. */
export function accessPicker({ id = 'access', current = null } = {}) {
  const level = h('select', { class: 'fui-field__input', id: `${id}-level` },
    Object.entries(LEVELS).filter(([k]) => k !== 'admins' || seesAll())
      .map(([k, label]) => h('option', { value: k, selected: (current ? current.level : 'private') === k }, label)));
  const team = h('select', { class: 'fui-field__input', id: `${id}-team`, 'aria-label': t('common.team'), hidden: true });
  const show = () => { team.hidden = level.value !== 'team'; };
  level.addEventListener('change', show);
  teams().then((list) => {
    team.replaceChildren(...list.map((team) => h('option', { value: String(team.id), selected: current && current.team && current.team.id === team.id }, team.name)));
    if (!list.length) level.querySelector('option[value="team"]')?.setAttribute('disabled', '');
    show();
  });
  const el = h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', for: `${id}-level` }, t('ownership.who_may_see')), h('div', { class: 'row' }, level, team),
    h('p', { class: 'fui-field__help' }, t('ownership.help')));
  return { el, value: () => ({ level: level.value, team_id: level.value === 'team' && team.value ? Number(team.value) : null }) };
}

/** The Access card of a container's, stack's, volume's, network's or image's page: whom it belongs to, changed by those
 *  who may manage it. */
export function accessCard({ path, current, reload }) {
  const body = h('div', { class: 'stack' }, facts([[t('ownership.who_may_see'), describe(current)], [t('ownership.owner'), current && current.user ? current.user.name : null]]));
  if (can('manage')) {
    const picker = accessPicker({ id: 'own', current });
    const save = button({ size: 'sm', onClick: async () => {
      save.disabled = true;
      try { await put(path, picker.value()); toast({ text: t('common.saved'), tone: 'good' }); reload(); } catch (e) { toast({ text: e.message, tone: 'critical' }); } finally { save.disabled = false; }
    } }, t('common.save'));
    body.append(picker.el, h('div', { class: 'row row--end' }, save));
  }
  return card({ title: t('ownership.access'), body });
}
