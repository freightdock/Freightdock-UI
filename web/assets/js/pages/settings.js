// Settings, for administrators. AI agents (MCP): the MCP server on or off for everybody, where it is, every API token
// and whose it is (revoked from here), and the CA to trust. Appearance: the FinUI preset everybody's Freightdock wears, a code made at FinUI create
// (finui.finstats.no/create) and pasted here. Sign-in: the providers (single sign-on and directories) and two-factor,
// drawn by signin-settings.js; the local sign-in always stays beside them.

import { h, icon, ago, when, num } from '../dom.js';
import { get, put, post, del } from '../api.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { claudeLine, caHelp } from './account.js';
import { status } from '../../finui/components/badge/badge.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { formField, textArea, inlineError } from '../../finui/components/field/field.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { button } from '../../finui/components/button/button.js';
import { callout } from '../../finui/components/callout/callout.js';
import { codeLine } from '../../finui/components/code-block/code-block.js';
import { toast } from '../../finui/components/toast/toast.js';
import { signinSettings } from './signin-settings.js';
import { facts } from '../../finui/components/facts/facts.js';
import { t } from '../i18n.js';


/** Draws the page in the stylesheet as it now is: the new one is loaded beside the old, which goes only once the new one
 *  is in, so nothing is drawn unstyled in between. The server ignores the query; it only makes the browser ask again. */
function restyle() {
  const old = document.querySelector('link[rel="stylesheet"][href^="/assets/finui.css"]');
  const next = h('link', { rel: 'stylesheet', href: `/assets/finui.css?v=${Date.now()}` });
  next.addEventListener('load', () => old?.remove(), { once: true });
  if (old) old.after(next); else document.head.append(next);
}

/** Where presets are made. Freightdock only takes the code. */
const CREATE = 'https://finui.finstats.no/create/';
/** The code in anything that holds one: the code itself, FinUI's install line, a FinUI create address. */
const codeIn = (text) => (/sh -s --\s+([0-9a-z]+)/i.exec(text) || /[?&]preset=([0-9a-z]+)/i.exec(text) || /^\s*([0-9a-z]*)\s*$/i.exec(text) || [])[1] ?? text.trim();

/** Appearance as FinStats has it: one row for the code, what it picks under it, and the way to FinUI create. */
function appearanceCard(cfg, presets) {
  let current = cfg.preset || '';
  const input = h('input', { class: 'fui-field__input mono', id: 'f-preset', type: 'text', autocomplete: 'off', spellcheck: false,
    value: current, placeholder: t('pages.settings.preset_placeholder'), 'aria-describedby': 'preset-help' });
  const err = h('div');
  const meaning = h('div', { class: 'fui-setting-row__notes' });
  const save = button({ size: 'sm' }, t('common.save'));
  const reset = button({ size: 'sm', variant: 'ghost' }, t('pages.settings.reset'));

  function paint() {
    const code = current;
    reset.hidden = !code;
    const chosen = presets.axes.map((a, i) => {
      const o = parseInt(code[i] || '0', 36);
      return o ? [a.label, a.options[o]?.label] : null;
    }).filter(Boolean);
    meaning.replaceChildren(
      chosen.length ? facts(chosen) : h('p', { class: 'fui-field__help' }, t('pages.settings.defaults')),
      h('p', { class: 'fui-field__help' }, button({ href: code ? `${CREATE}?preset=${code}` : CREATE, target: '_blank', rel: 'noopener noreferrer', size: 'sm' },
        icon('sliders', 14), code ? t('pages.settings.change_in_create') : t('pages.settings.make_in_create'), icon('external', 14))));
  }
  async function keep(code) {
    err.replaceChildren();
    input.removeAttribute('aria-invalid');
    save.disabled = true;
    try {
      await put('/api/settings/appearance', { preset: code });
      current = (await get('/api/settings/appearance')).preset || '';
      input.value = current;
      restyle();
      paint();
      toast({ text: t('pages.settings.saved_everybody'), tone: 'good' });
    } catch (e) {
      input.setAttribute('aria-invalid', 'true');
      err.replaceChildren(inlineError('preset-err', e.message));
      input.focus();
    } finally { save.disabled = false; }
  }
  save.addEventListener('click', () => keep(codeIn(input.value)));
  reset.addEventListener('click', () => keep(''));
  input.addEventListener('keydown', (e) => { if (e.key === 'Enter') { e.preventDefault(); save.click(); } });
  paint();

  return card({ title: t('pages.settings.appearance'), sub: t('pages.settings.appearance_sub'), body: h('div', null,
    h('div', { class: 'fui-setting-row__rows' }, settingRow({ id: 'preset', label: t('pages.settings.preset'), labelFor: 'f-preset',
      help: t('pages.settings.preset_help'),
      control: h('div', { class: 'fui-field__row' }, input, save, reset), error: err })),
    meaning) });
}


/** The template lists: addresses of Portainer-format JSON (v2 or v3), one a line. */
function templatesCard(cfg) {
  const urls = textArea({ id: 'tpl-urls', label: t('pages.settings.template_lists'), rows: 4, help: t('pages.settings.template_lists_help', { default: cfg.default }) });
  urls.input.value = (cfg.urls || []).join('\n');
  urls.input.classList.add('mono');
  const err = h('div');
  const save = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, urls.el, err, h('div', { class: 'row row--end' }, save));
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); err.replaceChildren(); save.disabled = true;
    try {
      await put('/api/settings/templates', { urls: urls.input.value.split('\n').map((x) => x.trim()).filter(Boolean) });
      toast({ text: t('common.saved'), tone: 'good' });
    } catch (x) { err.replaceChildren(callout({ tone: 'critical', body: x.message })); } finally { save.disabled = false; }
  });
  return card({ title: t('pages.templates.app_templates'), sub: t('pages.settings.templates_sub'), body: form });
}

/** A number field for a settings card, its value set. */
function numberField(id, label, value, help) {
  const f = formField({ id, label, type: 'number', inputMode: 'numeric', help });
  f.input.value = String(value);
  return f;
}

/** Saves a settings card: the fields' errors cleared, the one the server names marked, a toast for the rest. */
async function saveCard(button_, fields, path, body, ok) {
  for (const x of Object.values(fields)) x.setError('');
  button_.disabled = true;
  try { await put(path, body); toast({ text: ok, tone: 'good' }); }
  catch (err) { if (err.field && fields[err.field]) { fields[err.field].setError(err.message); fields[err.field].input.focus(); } else toast({ text: err.message, tone: 'critical' }); }
  finally { button_.disabled = false; }
}

function historyCard(cfg) {
  let enabled = !!cfg.enabled;
  const f = { every_s: numberField('every_s', t('pages.settings.every_s'), cfg.every_s, t('pages.settings.every_s_help')), keep_days: numberField('keep_days', t('pages.settings.keep_days'), cfg.keep_days, t('pages.settings.keep_days_help')) };
  const save = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true },
    settingRow({ id: 'history-on', label: t('pages.settings.history'), help: t('pages.settings.history_help'), control: toggle({ checked: enabled, labelledby: 'history-on-label', onChange: (v) => { enabled = v; } }) }),
    h('div', { class: 'grid-2' }, f.every_s.el, f.keep_days.el), h('div', { class: 'row row--end' }, save));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    saveCard(save, f, '/api/settings/history', { enabled, every_s: Number(f.every_s.input.value), keep_days: Number(f.keep_days.input.value) }, t('common.saved'));
  });
  return card({ title: t('pages.settings.history'), sub: t('pages.settings.history_sub'), body: form });
}

function updatesCard(cfg) {
  let enabled = !!cfg.enabled, auto = !!cfg.auto_update;
  const f = { check_hours: numberField('check_hours', t('pages.settings.check_hours'), cfg.check_hours, t('pages.settings.check_hours_help')) };
  const save = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const now = button({ onClick: async () => {
    now.disabled = true;
    toast({ text: t('pages.settings.asking') });
    try {
      const r = await post('/api/updates/check', { apply: false });
      toast({ text: t('pages.settings.checked', { hosts: t('pages.settings.n_hosts', { count: r.environments }), behind: t('common.n_containers', { count: r.outdated }) }), tone: 'good' });
    } catch (e) { toast({ text: e.message, tone: 'critical' }); } finally { now.disabled = false; }
  } }, t('pages.settings.check_now'));
  const form = h('form', { class: 'stack', novalidate: true },
    settingRow({ id: 'updates-on', label: t('pages.settings.look_newer'), help: t('pages.settings.look_newer_help'), control: toggle({ checked: enabled, labelledby: 'updates-on-label', onChange: (v) => { enabled = v; } }) }),
    settingRow({ id: 'auto-on', label: t('pages.settings.auto'), help: t('pages.settings.auto_help'), control: toggle({ checked: auto, labelledby: 'auto-on-label', onChange: (v) => { auto = v; } }) }),
    f.check_hours.el, h('div', { class: 'row row--end' }, now, save));
  form.addEventListener('submit', (e) => {
    e.preventDefault();
    saveCard(save, f, '/api/settings/updates', { enabled, auto_update: auto, check_hours: Number(f.check_hours.input.value) }, t('common.saved'));
  });
  return card({ title: t('pages.container.image_updates'), sub: t('pages.settings.updates_sub'), body: form });
}

/** AI agents (MCP), "MCP" 6: the switch for the whole instance, the address and the Claude Code line, every API token
 *  there is with its owner (an administrator revokes any of them here; each person makes their own in Account), and
 *  the CA to trust when Freightdock made its own certificate. */
function mcpCard(cfg, again) {
  const url = `${location.origin}${cfg.path}`;
  const sw = toggle({ checked: cfg.enabled, labelledby: 'mcp-on-label', describedby: 'mcp-on-help', onChange: async (on, revert) => {
    if (!on && cfg.active && !(await confirm({ title: t('pages.settings.mcp_off_title'), action: t('pages.account.turn_off'), body: t('pages.settings.mcp_off_body', { count: cfg.active }) }))) { revert(true); return; }
    try { await put('/api/settings/mcp', { enabled: on }); toast({ text: on ? t('pages.settings.mcp_on_done') : t('pages.settings.mcp_off_done'), tone: 'good' }); again(); }
    catch (e) { revert(!on); toast({ text: e.message, tone: 'critical' }); }
  } });
  const revoke = async (tok) => {
    if (!(await confirm({ title: t('pages.settings.revoke_title', { owner: tok.owner.name, name: tok.name }), action: t('pages.account.revoke'), body: t('pages.settings.revoke_body') }))) return;
    try { await del(`/api/settings/mcp/tokens/${tok.id}`); toast({ text: t('pages.account.revoked'), tone: 'good' }); again(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
  };
  const tokens = table({
    rows: cfg.tokens, filter: 'auto', label: t('pages.account.api_tokens'), empty: [t('pages.account.no_tokens'), t('pages.settings.no_tokens_text')],
    columns: [
      { label: t('common.name'), cell: (tok) => h('div', { class: 'name-cell' }, h('span', { class: 'truncate' }, tok.name), h('span', { class: 'sub mono' }, `fdk_…${tok.hint}`)), sort: (tok) => tok.name },
      { label: t('pages.settings.whose'), cell: (tok) => h('span', null, tok.owner.name, tok.owner.disabled ? h('span', { class: 'faint' }, ' ' + t('pages.settings.disabled_paren')) : null), sort: (tok) => tok.owner.name },
      { label: t('pages.account.may'), cell: (tok) => (tok.read_only ? status({ tone: 'info' }, t('pages.account.read_only')) : status({ tone: 'warning' }, t('pages.settings.owner_may'))) },
      { label: t('common.state'), cell: (tok) => (tok.active ? status({ tone: 'good' }, t('pages.users.active')) : status({ tone: 'critical' }, tok.owner.disabled ? t('pages.settings.owner_disabled') : t('pages.settings.expired'))), sort: (tok) => (tok.active ? 1 : 0) },
      { label: t('pages.account.last_used'), cell: (tok) => (tok.last_used_at ? h('span', { class: 'nowrap', title: tok.last_used_ip ? t('pages.account.used_from', { when: when(tok.last_used_at), ip: tok.last_used_ip }) : when(tok.last_used_at) }, ago(tok.last_used_at)) : h('span', { class: 'faint' }, t('common.never'))), sort: (tok) => tok.last_used_at || 0 },
      { label: t('pages.account.expires'), cell: (tok) => (tok.expires_at ? h('span', { class: 'nowrap' }, when(tok.expires_at)) : h('span', { class: 'faint' }, t('common.never'))), sort: (tok) => tok.expires_at || Infinity },
      { label: '', nosort: true, cell: (tok) => h('div', { class: 'cell-actions' }, button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.settings.revoke_label', { owner: tok.owner.name, name: tok.name }), onClick: () => revoke(tok) }, icon('trash', 14))) },
    ],
  });
  return card({ title: t('pages.settings.mcp'), sub: t('pages.settings.mcp_sub'), body: h('div', { class: 'stack' },
    h('div', { class: 'fui-setting-row__rows' }, settingRow({ id: 'mcp-on', label: t('pages.settings.mcp_server'), help: t('pages.settings.mcp_help'), control: sw })),
    facts([[t('pages.account.mcp_address'), url, { wide: true, mono: true }], [t('pages.settings.active_tokens'), num(cfg.active)], [t('pages.account.signs_in_with'), t('pages.account.signs_in_with_value')]]),
    h('span', { class: 'faint' }, t('pages.settings.connect_claude')),
    codeLine(claudeLine(url, '<token>'), { label: t('webhook.copy_command') }),
    h('div', { class: 'row' }, button({ href: '/account', size: 'sm' }, icon('key', 14), t('pages.settings.own_token')), h('span', { class: 'faint' }, t('pages.settings.each_person'))),
    caHelp(cfg.ca),
    tokens) });
}

/** Where moving over from Portainer starts. */
function migrateCard() {
  return card({ title: t('pages.settings.migrate'), sub: t('pages.settings.migrate_sub'), body: h('div', { class: 'stack' },
    h('p', null, t('pages.settings.migrate_body')),
    h('div', { class: 'row row--end' }, button({ href: '/portainer' }, t('routes.portainer')))) });
}

export function settingsPage({ el, signal }) {
  const look = h('div'), sso = h('div'), twoFactor = h('div'), hist = h('div'), upd = h('div'), tpl = h('div'), mcp = h('div');
  el.append(pageHeader(t('nav.settings'), t('pages.settings.sub')), h('div', { class: 'stack' }, look, upd, hist, sso, twoFactor, mcp, tpl, migrateCard()));
  loadInto(mcp, { signal, load: (s) => get('/api/settings/mcp', { signal: s }), draw: mcpCard });
  loadInto(upd, { signal, load: (s) => get('/api/settings/updates', { signal: s }), draw: updatesCard });
  // The axes' names come from FinUI's own presets.json, so what a code picks reads as FinUI create says it.
  loadInto(look, { signal, load: (s) => Promise.all([get('/api/settings/appearance', { signal: s }), fetch('/assets/finui/create/presets.json', { signal: s }).then((r) => r.json())]),
    draw: ([cfg, presets]) => appearanceCard(cfg, presets) });
  loadInto(hist, { signal, load: (s) => get('/api/settings/history', { signal: s }), draw: historyCard });
  signinSettings(sso, twoFactor, signal);
  loadInto(tpl, { signal, load: (s) => get('/api/settings/templates', { signal: s }), draw: templatesCard });
}
