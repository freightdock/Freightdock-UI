// Import from Portainer, for administrators: connect (Portainer's address and an access token, kept sealed; its
// certificate checked, pinned to one pasted, or not checked by an explicit choice), read what Portainer has (GET only,
// nothing there changes), match its environments to Freightdock's, tick what to bring, import, and see how each thing went.
// Stacks are adopted in place under the same Compose project, so their containers keep running; teams come with the
// members Freightdock has a user of the same name for.

import { h, mount } from '../dom.js';
import { get, post, put, del } from '../api.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { showJoin } from './environments.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { formField, textArea } from '../../finui/components/field/field.js';
import { checkbox } from '../../finui/components/choice/choice.js';
import { callout } from '../../finui/components/callout/callout.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { status } from '../../finui/components/badge/badge.js';
import { errorState } from '../../finui/components/error/error.js';
import { toast } from '../../finui/components/toast/toast.js';
import { sk } from '../../finui/components/skeleton/skeleton.js';
import { icon } from '../../finui/core.js';
import { t, tOr } from '../i18n.js';



/** The form that connects (or reconnects) to a Portainer. */
function connectForm(conn, after) {
  const url = formField({ id: 'pt-url', label: t('pages.portainer.address'), placeholder: 'https://portainer.example:9443', autocomplete: 'off',
    help: t('pages.portainer.address_help') });
  const token = formField({ id: 'pt-token', label: t('pages.notifications.access_token'), type: 'password', autocomplete: 'new-password',
    help: t('pages.portainer.token_help') });
  const ca = textArea({ id: 'pt-ca', label: t('pages.portainer.cert'), rows: 4, placeholder: '-----BEGIN CERTIFICATE-----',
    help: t('pages.portainer.cert_help') });
  let skip = !!(conn && conn.skip_verify);
  const skipBox = checkbox({ label: t('pages.portainer.skip'), checked: skip,
    help: t('pages.portainer.skip_help'), onChange: (v) => { skip = v; } });
  if (conn && conn.connected) {
    url.input.value = conn.url;
    ca.input.value = conn.ca || '';
    token.input.placeholder = t('pages.registries.kept');
  }
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.portainer.try_save'));
  const form = h('form', { class: 'stack', novalidate: true }, url.el, token.el, ca.el, skipBox, h('div', { class: 'row row--end' }, go));
  const fields = { url, token, ca };
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    for (const f of Object.values(fields)) if (f.setError) f.setError('');
    go.disabled = true;
    try {
      await put('/api/portainer', { url: url.input.value, token: token.input.value, ca: ca.input.value, skip_verify: skip });
      toast({ text: t('pages.portainer.connected'), tone: 'good' });
      after();
    } catch (err) {
      const f = (err.field && fields[err.field]) || url;
      if (f.setError) f.setError(err.message); else toast({ text: err.message, tone: 'critical' });
      f.input.focus();
    } finally { go.disabled = false; }
  });
  return form;
}

/** What is ticked, kept across reads: `kind:id` to on or off, and a Git stack's token. */
function picks() {
  const on = new Map();
  const tokens = new Map();
  return {
    is: (kind, id, dflt) => (on.has(`${kind}:${id}`) ? on.get(`${kind}:${id}`) : dflt),
    set: (kind, id, v) => on.set(`${kind}:${id}`, v),
    token: (id) => tokens.get(id) || '',
    setToken: (id, v) => tokens.set(id, v),
  };
}

function outcomeBadge(o) {
  const tone = { done: 'good', skipped: 'info', failed: 'critical' }[o.outcome] || 'info';
  return status({ tone }, tOr(`pages.portainer.outcome.${o.outcome}`, o.outcome));
}

const KIND_WORDS = { stack: t('routes.stack'), group: t('pages.home.group'), tag: t('common.tag'), registry: t('routes.registry'), template: t('routes.template'), people: t('grants.people'), team: t('common.team') };

function results(list) {
  const n = (w) => list.filter((o) => o.outcome === w).length;
  return card({ title: t('pages.portainer.how_it_went'), sub: t('pages.portainer.tally', { done: n('done'), skipped: n('skipped'), failed: n('failed') }), body: table({
    rows: list, filter: false, empty: [t('pages.portainer.nothing_ticked'), t('pages.portainer.nothing_ticked_text')],
    columns: [
      { label: t('pages.events.what'), cell: (o) => KIND_WORDS[o.kind] || o.kind },
      { label: t('common.name'), cell: (o) => (o.kind === 'stack' && o.freightdock_id ? h('span', { class: 'truncate' }, o.name) : h('span', { class: 'truncate' }, o.name || `#${o.id}`)) },
      { label: t('pages.signins.outcome'), cell: outcomeBadge },
      { label: t('pages.portainer.why'), cell: (o) => h('span', { class: 'faint' }, o.reason || '') },
    ],
  }) });
}

/** The page drawn from a read: environments to match, then each kind of thing with its boxes, then Import. */
function preview(scan, ctx) {
  const { chosen, mapping, rescan, resultSlot } = ctx;
  const envName = (id) => (scan.freightdock.find((e) => e.id === id) || {}).name || t('pages.portainer.environment_n', { id });
  const ptName = (id) => (scan.environments.find((e) => e.id === id) || {}).name || t('pages.portainer.environment_n', { id });

  const newAgent = async (pe) => {
    const ok = await confirm({ title: t('pages.portainer.new_agent_title', { name: pe.name }), action: t('webhook.make_it'),
      body: t('pages.portainer.new_agent_body') });
    if (!ok) return;
    try {
      const r = await post('/api/environments', { name: pe.name, kind: 'agent' });
      mapping[pe.id] = r.environment.id;
      showJoin(r.environment.name, r.join);
      rescan();
    } catch (e) { toast({ text: e.message, tone: 'critical' }); }
  };

  const envSelect = (pe) => {
    const sel = h('select', { class: 'fui-field__input', 'aria-label': t('pages.portainer.env_for', { name: pe.name }) },
      h('option', { value: '', selected: pe.env == null }, t('pages.portainer.not_imported')),
      scan.freightdock.map((s) => h('option', { value: String(s.id), selected: pe.env === s.id }, s.name)),
      h('option', { value: 'new' }, t('pages.portainer.new_agent_option')));
    sel.addEventListener('change', () => {
      if (sel.value === 'new') { sel.value = pe.env == null ? '' : String(pe.env); newAgent(pe); return; }
      mapping[pe.id] = sel.value === '' ? null : Number(sel.value);
      rescan();
    });
    return sel;
  };

  const envCard = card({ title: t('nav.environments'), sub: t('pages.portainer.env_sub'), body: table({
    rows: scan.environments, filter: false,
    columns: [
      { label: t('pages.portainer.in_portainer'), cell: (e) => h('div', { class: 'stack stack--tight' }, h('strong', null, e.name), h('span', { class: 'faint mono truncate' }, e.url)) },
      { label: t('pages.networks.kind'), cell: (e) => e.kind },
      { label: t('common.state'), cell: (e) => status({ tone: e.status === 'up' ? 'good' : 'warning' }, tOr(`docker.env_status.${e.status}`, e.status)) },
      { label: t('pages.portainer.group_tags'), cell: (e) => h('span', { class: 'faint' }, [e.group, ...(e.tags || [])].filter(Boolean).join(', ') || '–') },
      { label: t('pages.portainer.in_freightdock'), cell: (e) => (e.docker ? envSelect(e) : h('span', { class: 'faint' }, t('pages.portainer.not_docker'))) },
      { label: t('pages.portainer.matched_by'), cell: (e) => h('span', { class: 'faint' }, e.match && e.env === e.match.env ? e.match.by : e.env != null ? t('pages.portainer.you') : e.info_error ? t('pages.portainer.nothing_because', { reason: e.info_error }) : t('pages.volumes.nothing')) },
    ],
  }) });

  const box = (kind, item, dflt, disabled) => {
    const b = checkbox({ label: '', checked: !disabled && chosen.is(kind, item.id, dflt), disabled, onChange: (v) => chosen.set(kind, item.id, v) });
    b.querySelector('input').setAttribute('aria-label', t('pages.portainer.import_name', { name: item.name || item.title }));
    return b;
  };

  const stackRows = scan.stacks.filter((s) => s.kind !== 'kubernetes');
  const stacksCard = card({ title: t('nav.stacks'), sub: t('pages.portainer.stacks_sub', { can: stackRows.filter((s) => s.importable).length, count: stackRows.length }), body: table({
    rows: stackRows, label: t('pages.stacks.filter'),
    columns: [
      { label: '', nosort: true, cell: (s) => box('stack', s, s.checked, !s.importable) },
      { label: t('routes.stack'), cell: (s) => h('div', { class: 'stack stack--tight' }, h('strong', null, s.name), h('span', { class: 'faint' }, t(s.active ? 'pages.portainer.stack_line_active' : 'pages.portainer.stack_line_inactive', { kind: { compose: 'Compose', git: 'Git', swarm: 'Swarm' }[s.kind] || s.kind, vars: t('pages.container.variables', { count: s.vars }) }))) },
      { label: t('pages.stacks.from'), cell: (s) => ptName(s.endpoint_id) },
      { label: t('pages.notifications.to'), cell: (s) => (s.env != null ? envName(s.env) : h('span', { class: 'faint' }, '–')) },
      { label: t('pages.portainer.running_there'), sort: (s) => s.running, cell: (s) => (s.containers ? status({ tone: s.running === s.containers ? 'good' : 'warning', line: true }, t('pages.portainer.n_of', { n: s.running, total: s.containers })) : h('span', { class: 'faint' }, t('pages.volumes.nothing'))) },
      { label: t('pages.portainer.notes'), nosort: true, cell: (s) => {
        const lines = [s.reason, ...(s.importable ? s.notes : [])].filter(Boolean).map((n) => h('span', { class: s.importable ? 'faint' : null }, n));
        let tok = null;
        if (s.importable && s.needs_token) {
          tok = h('input', { class: 'fui-field__input', type: 'password', autocomplete: 'off', placeholder: t('pages.portainer.repo_token'), 'aria-label': t('pages.portainer.git_token_for', { name: s.name }), value: chosen.token(s.id) });
          tok.addEventListener('input', () => chosen.setToken(s.id, tok.value));
        }
        return h('div', { class: 'stack stack--tight' }, lines, tok);
      } },
    ],
  }) });

  const simpleCard = (title, kind, rows, describe) => (rows.length ? card({ title, body: table({
    rows, filter: false,
    columns: [
      { label: '', nosort: true, cell: (r) => box(kind, r, r.importable !== false && !r.imported, r.importable === false || !!r.imported) },
      { label: t('common.name'), cell: (r) => h('strong', null, r.name || r.title) },
      { label: t('pages.portainer.what_it_makes'), nosort: true, cell: (r) => h('span', { class: 'faint' }, describe(r)) },
    ],
  }) }) : null);

  const members = (ids) => ids.map((id) => {
    const pe = scan.environments.find((e) => e.id === id);
    return pe && pe.env != null ? envName(pe.env) : null;
  }).filter(Boolean);
  const groupsCard = simpleCard(t('pages.env_groups.groups'), 'group', scan.groups, (g) => (g.imported ? t('pages.portainer.imported_already') : t('pages.portainer.group_line', { members: members(g.environments).join(', ') || t('pages.portainer.no_matched') })));
  const tagsCard = simpleCard(t('pages.images.tags'), 'tag', scan.tags, (tag) => (tag.imported ? t('pages.portainer.imported_already') : t('pages.portainer.tag_line', { members: members(tag.environments).join(', ') || t('pages.portainer.no_matched') })));
  const regCard = simpleCard(t('nav.registries'), 'registry', scan.registries, (r) => r.reason || (r.needs_password ? t('pages.portainer.registry_line_password', { kind: r.kind, host: r.host, user: r.username }) : t('pages.portainer.registry_line', { kind: r.kind, host: r.host })));
  const tplCard = simpleCard(t('pages.portainer.custom_templates'), 'template', scan.templates, (tpl) => tpl.reason || (tpl.variables ? t('pages.portainer.template_line_vars', { vars: t('pages.container.variables', { count: tpl.variables }) }) : t('pages.portainer.template_line')));

  // Teams: made or matched, with the members Freightdock has a user for ("Portainer" 10). Ticked to begin with when the
  // import would change something.
  const teamRows = scan.teams || [];
  const joining = (team) => team.members.filter((m) => m.freightdock_user_id != null && !m.already);
  const teamDefault = (team) => team.importable && (team.outcome === 'new' || joining(team).length > 0);
  const teamWhat = (team) => team.reason || { new: t('pages.portainer.team_new'), exists: t('pages.portainer.team_exists'), imported: t('pages.portainer.team_imported') }[team.outcome] || '';
  const teamMembers = (team) => {
    const names = (list) => list.map((m) => m.name).join(', ');
    const add = joining(team), there = team.members.filter((m) => m.already), none = team.members.filter((m) => m.freightdock_user_id == null);
    return h('div', { class: 'stack stack--tight' },
      add.length ? h('span', null, t('pages.portainer.members_added', { names: names(add) })) : null,
      there.length ? h('span', { class: 'faint' }, t('pages.portainer.members_there', { names: names(there) })) : null,
      none.length ? h('span', { class: 'faint' }, t('pages.portainer.members_none', { names: names(none) })) : null,
      team.members.length ? null : h('span', { class: 'faint' }, t('pages.portainer.nobody_in_it')));
  };
  const teamsCard = teamRows.length ? card({ title: t('grants.teams'), sub: t('pages.portainer.teams_sub'), body: table({
    rows: teamRows, filter: false,
    columns: [
      { label: '', nosort: true, cell: (team) => box('team', team, teamDefault(team), !team.importable) },
      { label: t('common.team'), cell: (team) => h('strong', null, team.name) },
      { label: t('pages.portainer.in_freightdock'), nosort: true, cell: (team) => h('span', { class: 'faint' }, teamWhat(team)) },
      { label: t('pages.users.members'), nosort: true, cell: teamMembers },
    ],
  }) }) : null;

  let people = chosen.is('people', 0, scan.people.users > 0);
  let deploy = chosen.is('deploy', 0, false);
  const peopleBox = scan.people.users || scan.people.teams ? checkbox({ label: t('pages.portainer.keep_people', { users: t('pages.portainer.n_users', { count: scan.people.users }), teams: t('pages.portainer.n_teams', { count: scan.people.teams }) }),
    help: t('pages.portainer.people_help'), checked: people, onChange: (v) => { people = v; chosen.set('people', 0, v); } }) : null;
  const deployBox = checkbox({ label: t('pages.portainer.deploy_each'), help: t('pages.portainer.deploy_each_help'),
    checked: deploy, onChange: (v) => { deploy = v; chosen.set('deploy', 0, v); } });

  const go = button({ variant: 'primary' }, icon('download', 14), t('pages.portainer.import_ticked'));
  go.addEventListener('click', async () => {
    const ticked = (kind, rows, ok, dflt = () => true) => rows.filter((r) => ok(r) && chosen.is(kind, r.id, kind === 'stack' ? r.checked : dflt(r))).map((r) => r.id);
    const body = {
      mapping,
      stacks: ticked('stack', scan.stacks, (s) => s.importable).map((id) => ({ id, token: chosen.token(id) || null })),
      groups: ticked('group', scan.groups, (g) => !g.imported),
      tags: ticked('tag', scan.tags, (tag) => !tag.imported),
      registries: ticked('registry', scan.registries, (r) => r.importable),
      templates: ticked('template', scan.templates, (tpl) => tpl.importable),
      teams: ticked('team', teamRows, (team) => team.importable, teamDefault),
      people, deploy,
    };
    go.disabled = true;
    mount(resultSlot, sk.tableRows(4));
    try {
      const r = await post('/api/portainer/import', body);
      mount(resultSlot, results(r.results));
      resultSlot.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      rescan();
    } catch (e) { mount(resultSlot, errorState(e)); } finally { go.disabled = false; }
  });

  const errs = Object.entries(scan.errors || {});
  return h('div', { class: 'stack' },
    errs.length ? callout({ tone: 'warning', title: t('pages.portainer.unreadable'), body: errs.map(([k, v]) => `${k}: ${v}`).join(' ') }) : null,
    envCard, stacksCard, groupsCard, tagsCard, regCard, tplCard, teamsCard,
    card({ title: t('pages.portainer.import'), body: h('div', { class: 'stack' }, peopleBox, deployBox, h('div', { class: 'row row--end' }, go)) }));
}

export function portainerPage({ el, signal }) {
  const connSlot = h('div');
  const scanSlot = h('div');
  const resultSlot = h('div');
  el.append(pageHeader(t('routes.portainer'), t('pages.portainer.sub')), h('div', { class: 'stack' }, connSlot, resultSlot, scanSlot));
  const chosen = picks();
  const mapping = {};

  const rescan = async () => {
    if (!scanSlot.firstChild) mount(scanSlot, sk.tableRows(6));
    try {
      const scan = await post('/api/portainer/scan', { mapping }, { signal });
      mount(scanSlot, preview(scan, { chosen, mapping, rescan, resultSlot }));
    } catch (e) {
      if (!signal.aborted) mount(scanSlot, errorState(e, rescan));
    }
  };

  const reload = loadInto(connSlot, {
    signal,
    load: (s) => get('/api/portainer', { signal: s }),
    draw: (conn, again) => {
      if (!conn.connected) {
        mount(scanSlot, '');
        return card({ title: t('pages.portainer.connect'), sub: t('pages.portainer.editions'), body: connectForm(conn, again) });
      }
      const change = button({ size: 'sm', onClick: () => mount(connSlot, card({ title: t('pages.portainer.change_conn'), body: connectForm(conn, again) })) }, t('pages.portainer.change'));
      const forget = button({ size: 'sm', onClick: async () => {
        const ok = await confirm({ title: t('pages.portainer.forget_title'), action: t('pages.portainer.forget_it'), body: t('pages.portainer.forget_body') });
        if (!ok) return;
        try { await del('/api/portainer'); again(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
      } }, t('pages.portainer.forget'));
      const read = button({ size: 'sm', variant: 'primary', onClick: () => { mount(scanSlot, sk.tableRows(6)); rescan(); } }, icon('refresh', 14), t('pages.portainer.read_again'));
      if (!scanSlot.firstChild) rescan();
      return card({ title: 'Portainer', sub: conn.url, actions: h('div', { class: 'row' }, read, change, forget), body: h('div', { class: 'stack' },
        facts([[t('pages.portainer.version'), conn.version], [t('pages.portainer.certificate'), conn.trust], [t('pages.stack_git.token'), conn.has_token ? t('pages.portainer.kept_sealed') : t('common.none_word')]]),
        conn.skip_verify ? callout({ tone: 'warning', body: t('pages.portainer.not_checked') }) : null) });
    },
  });
  void reload;
}
