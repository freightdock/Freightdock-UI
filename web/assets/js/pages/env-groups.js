// Environment groups and tags: the fields that place a host (its group, its tags), and the two cards on the
// environments page that list them and, for an administrator, add, rename and remove them.

import { h } from '../dom.js';
import { get, post, put, del } from '../api.js';
import { isAdmin } from '../state.js';
import { grantsDialog } from '../grants.js';
import { securityDialog } from '../security.js';
import { confirm } from '../confirm.js';
import { card } from '../../finui/components/card/card.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { choiceGroup } from '../../finui/components/choice/choice.js';
import { tagInput } from '../../finui/components/tag-input/tag-input.js';
import { chip, chipSet } from '../../finui/components/chip/chip.js';
import { emptyState } from '../../finui/components/empty/empty.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

/** A host's group and tags as fields: `el`, and `value()` for the body of a save ({ group_id, tags }). */
export function placementFields(env, groups, tags) {
  const select = h('select', { class: 'fui-field__input', id: 'env-group' },
    h('option', { value: '' }, t('pages.env_groups.no_group')),
    groups.map((g) => h('option', { value: String(g.id), selected: !!env.group && env.group.id === g.id }, g.name)));
  let chosen = [...(env.tags || [])];
  const known = tags.map((tag) => tag.name).filter((n) => !chosen.some((c) => c.toLowerCase() === n.toLowerCase()));
  const el = h('div', { class: 'stack' },
    h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', for: 'env-group' }, t('pages.home.group')), select,
      h('p', { class: 'fui-field__help' }, groups.length ? t('pages.env_groups.one_group') : t('pages.env_groups.no_groups_yet'))),
    h('div', { class: 'fui-field' }, h('span', { class: 'fui-field__label' }, t('pages.images.tags')),
      tagInput({ label: t('pages.images.tags'), value: chosen, placeholder: t('pages.env_groups.add_tag_placeholder'), onChange: (v) => { chosen = v; } }),
      h('p', { class: 'fui-field__help' }, known.length ? t('pages.env_groups.tags_help_known', { known: known.join(', ') }) : t('pages.env_groups.tags_help'))));
  return { el, value: () => ({ group_id: select.value ? Number(select.value) : null, tags: chosen }) };
}

/** The groups and tags there are, for a dialog that places a host. */
export async function placementLists(signal) {
  const [groups, tags] = await Promise.all([get('/api/environment-groups', { signal }), get('/api/tags', { signal })]);
  return { groups, tags };
}

function groupDialog(group, envs, after) {
  const name = formField({ id: 'group-name', label: t('common.name'), placeholder: t('pages.env_groups.production'), autocomplete: 'off' });
  const about = formField({ id: 'group-description', label: t('pages.templates.description'), placeholder: t('pages.env_groups.description_placeholder'), autocomplete: 'off' });
  name.input.value = group ? group.name : '';
  about.input.value = group ? group.description : '';
  let members = group ? [...group.environments].map(String) : [];
  const elsewhere = (e) => e.group && (!group || e.group.id !== group.id);
  const hosts = envs.length ? choiceGroup({ label: t('pages.env_groups.hosts_in_it'), kind: 'checkbox', value: members, onChange: (v) => { members = v; },
    options: envs.map((e) => ({ value: String(e.id), label: e.name, help: elsewhere(e) ? t('pages.env_groups.moves_here', { name: e.group.name }) : null })) }) : null;
  const go = button({ variant: 'primary', type: 'submit' }, group ? t('common.save') : t('common.add'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, about.el, hosts, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: group ? t('routes.edit', { name: group.name }) : t('pages.env_groups.add_group_title'), body: form, initialFocus: name.input });
  const fields = { name, description: about };
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const f of Object.values(fields)) f.setError('');
    go.disabled = true;
    const body = { name: name.input.value, description: about.input.value, environments: members.map(Number) };
    try {
      if (group) await put(`/api/environment-groups/${group.id}`, body); else await post('/api/environment-groups', body);
      m.close(); toast({ text: group ? t('common.saved') : t('pages.environments.added'), tone: 'good' }); after();
    } catch (err) {
      (fields[err.field] || name).setError(err.message);
    } finally { go.disabled = false; }
  });
}

function tagDialog(tag, after) {
  const name = formField({ id: 'tag-name', label: t('common.name'), placeholder: 'gpu', autocomplete: 'off', help: t('pages.env_groups.tag_help') });
  name.input.value = tag ? tag.name : '';
  const go = button({ variant: 'primary', type: 'submit' }, tag ? t('pages.rename.rename') : t('common.add'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: tag ? t('pages.rename.title', { name: tag.name }) : t('pages.env_groups.add_tag_title'), body: form, initialFocus: name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    name.setError('');
    try {
      if (tag) await put(`/api/tags/${tag.id}`, { name: name.input.value }); else await post('/api/tags', { name: name.input.value });
      m.close(); after();
    } catch (err) { name.setError(err.message); }
  });
}

async function removeOne(what, path, body, after) {
  if (!await confirm({ title: t('common.remove_name', { name: what }), body })) return;
  try { await del(path); toast({ text: t('common.removed'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

const hostsOf = (ids, envs) => ids.map((id) => (envs.find((e) => e.id === id) || {}).name).filter(Boolean);

/** The groups card: each group with its hosts; add, edit, remove for an administrator. */
export function groupsCard(groups, envs, after) {
  const admin = isAdmin();
  const rows = groups.map((g) => h('li', { class: 'fd-list__row' },
    h('div', { class: 'name-cell grow' }, h('strong', null, g.name), g.description ? h('span', { class: 'sub' }, g.description) : null,
      h('span', { class: 'sub' }, g.environments.length ? hostsOf(g.environments, envs).join(', ') : t('pages.env_groups.no_hosts_yet'))),
    admin ? h('div', { class: 'cell-actions' },
      button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.environments.who_has_role', { name: g.name }), title: t('ownership.access'), onClick: () => grantsDialog({ title: t('pages.environments.access_to', { name: g.name }), path: `/api/environment-groups/${g.id}/access` }) }, icon('users', 14)),
      button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.environments.security_of', { name: g.name }), title: t('pages.environments.security_settings'), onClick: () => securityDialog({ title: t('pages.environments.security_on', { name: g.name }), path: `/api/environment-groups/${g.id}/security`, group: true }) }, icon('shield', 14)),
      button({ variant: 'icon', size: 'sm', 'aria-label': t('routes.edit', { name: g.name }), onClick: () => groupDialog(g, envs, after) }, icon('pencil', 14)),
      button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: g.name }), onClick: () => removeOne(g.name, `/api/environment-groups/${g.id}`, t('pages.env_groups.remove_group_body'), after) }, icon('trash', 14))) : null));
  return card({ title: t('pages.env_groups.groups'), sub: t('pages.env_groups.groups_sub'),
    actions: admin ? button({ size: 'sm', onClick: () => groupDialog(null, envs, after) }, icon('plus', 14), t('pages.env_groups.add_group')) : null,
    body: rows.length ? h('ul', { class: 'fd-list' }, rows) : emptyState(t('pages.env_groups.no_groups'), t('pages.env_groups.no_groups_text')) });
}

/** The tags card: every tag with how many hosts have it; add, rename, remove for an administrator. */
export function tagsCard(tags, envs, after) {
  const admin = isAdmin();
  const rows = tags.map((tag) => h('li', { class: 'fd-list__row' },
    h('div', { class: 'name-cell grow' }, chipSet(chip(tag.name)),
      h('span', { class: 'sub' }, tag.environments.length ? hostsOf(tag.environments, envs).join(', ') : t('pages.env_groups.on_no_host'))),
    admin ? h('div', { class: 'cell-actions' },
      button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.rename.title', { name: tag.name }), onClick: () => tagDialog(tag, after) }, icon('pencil', 14)),
      button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: tag.name }), onClick: () => removeOne(tag.name, `/api/tags/${tag.id}`, t('pages.env_groups.remove_tag_body'), after) }, icon('trash', 14))) : null));
  return card({ title: t('pages.images.tags'), sub: t('pages.env_groups.tags_sub'),
    actions: admin ? button({ size: 'sm', onClick: () => tagDialog(null, after) }, icon('plus', 14), t('pages.env_groups.add_tag')) : null,
    body: rows.length ? h('ul', { class: 'fd-list' }, rows) : emptyState(t('pages.env_groups.no_tags'), t('pages.env_groups.no_tags_text')) });
}
