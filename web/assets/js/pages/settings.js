// Settings, for administrators. Appearance: the FinUI preset everybody's Freightdock wears, a code made at FinUI create
// (finui.finstats.no/create) and pasted here. Sign-in: the providers (single sign-on and directories) and two-factor,
// drawn by signin-settings.js; the local sign-in always stays beside them.

import { h, icon, ago, when, num } from '../dom.js';
import { get, put, post, del } from '../api.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
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


export function settingsPage({ el, signal }) {
  const look = h('div'), sso = h('div'), twoFactor = h('div');
  el.append(pageHeader(t('nav.settings'), t('pages.settings.sub')), h('div', { class: 'stack' }, look, sso, twoFactor));
  // The axes' names come from FinUI's own presets.json, so what a code picks reads as FinUI create says it.
  loadInto(look, { signal, load: (s) => Promise.all([get('/api/settings/appearance', { signal: s }), fetch('/assets/finui/create/presets.json', { signal: s }).then((r) => r.json())]),
    draw: ([cfg, presets]) => appearanceCard(cfg, presets) });
  signinSettings(sso, twoFactor, signal);
}
