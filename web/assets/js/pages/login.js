// Sign in, then on to where the person was going (Home, with no host chosen yet, unless a link said where). Each
// sign-in provider that is on has its button first, and the password form stays beneath them: the way in when a
// provider is down, and where a directory's (LDAP) password is typed too. A password that wants a second factor is
// followed by the code step, or by setting one up when an administrator requires it.

import { h } from '../dom.js';
import { post } from '../api.js';
import { state, setEnv } from '../state.js';
import { navigate } from '../router.js';
import { authCard, credentialsForm } from './auth.js';
import { codeField, setupBox, showRecoveryCodes } from './two-factor.js';
import { button } from '../../finui/components/button/button.js';
import { callout } from '../../finui/components/callout/callout.js';
import { spinner } from '../../finui/components/spinner/spinner.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

/** Only an address inside the app: `?next=//evil.example` goes home instead. */
export function safeNext(next) {
  return typeof next === 'string' && next.startsWith('/') && !next.startsWith('//') && !next.startsWith('/\\') ? next : '/home';
}

const ICONS = { github: 'github', google: 'globe', microsoft: 'grid' };

async function done(next) {
  const { refreshMe } = await import('../app.js');
  await refreshMe();
  navigate(next, { replace: true });
}

/** The second step: a code for the ticket the password earned, or, when one is required and there is none, setting one
 *  up first. */
async function secondStep(slot, r, next) {
  const enrol = r.second_factor === 'enrol';
  const code = codeField('totp-code', { recovery: !enrol });
  const general = h('div', { class: 'auth-general' });
  const go = button({ variant: 'primary', type: 'submit', block: true }, enrol ? t('login.turn_on_and_sign_in') : t('routes.login'));
  const back = button({ variant: 'ghost', block: true, onClick: () => location.reload() }, t('login.start_again'));
  let body = code.el;
  if (enrol) {
    slot.replaceChildren(spinner(18));
    try {
      const made = await post('/api/login/totp/enrol', { ticket: r.ticket });
      body = h('div', { class: 'stack' }, callout({ tone: 'info', body: t('login.required') }), setupBox(made, code));
    } catch (e) {
      slot.replaceChildren(callout({ tone: 'critical', body: e.message }), back);
      return;
    }
  }
  const form = h('form', { class: 'auth-form auth-step', novalidate: true },
    enrol ? null : h('p', null, t('login.type_code')), body, general, go, back);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    code.setError(''); general.replaceChildren();
    go.disabled = true;
    try {
      const out = await post('/api/login/totp', { ticket: r.ticket, code: code.input.value }, { quiet401: true });
      if (out && out.recovery_codes) showRecoveryCodes(out.recovery_codes, { onClose: () => done(next) });
      else await done(next);
    } catch (err) {
      if (err.field === 'code') { code.setError(err.message); code.input.select(); } else general.replaceChildren(callout({ tone: 'critical', body: err.message }));
    } finally { go.disabled = false; }
  });
  slot.replaceChildren(form);
  queueMicrotask(() => code.input.focus());
}

export function loginPage({ el, query }) {
  // Whoever signs in starts at Home and chooses a host there.
  setEnv(null);
  const next = safeNext(query.get('next'));
  const sso = (state.status && state.status.sso) || { providers: [], ldap: false };
  const buttons = sso.providers || [];
  const failed = query.get('sso_error');
  const slot = h('div');
  const form = credentialsForm({
    action: t('routes.login'), primary: !buttons.length,
    submit: async (c) => {
      const r = await post('/api/login', c, { quiet401: true });
      if (r && r.second_factor) await secondStep(slot, r, next);
      else await done(next);
    },
  });
  slot.append(
    buttons.length ? h('div', { class: 'auth-sso' },
      ...buttons.map((p, i) => button({ variant: i === 0 ? 'primary' : 'default', block: true, href: `/api/auth/sso/${encodeURIComponent(p.id)}/login?next=${encodeURIComponent(next)}` },
        icon(ICONS[p.preset] || 'key', 15), t('login.with', { name: p.label }))),
      h('p', { class: 'auth-or' }, h('span', null, sso.ldap ? t('login.or_directory') : t('login.or_password')))) : null,
    form);
  el.append(authCard(t('routes.login'), null, failed ? callout({ tone: 'critical', body: failed }) : null, slot));
}
