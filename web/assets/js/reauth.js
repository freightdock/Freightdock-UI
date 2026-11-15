// The password again ("Sign-in" 18): a shell, an upgrade of the cluster and a binary for it want a sign-in from the
// last ten minutes. `askPassword` is the dialog that gives it (with the code, when two-factor is on); `ensureRecent`
// asks first only when the session is not recent, for what is costly to send twice (a binary of many megabytes).

import { h } from './dom.js';
import { get, post } from './api.js';
import { state } from './state.js';
import { navigate } from './router.js';
import { openModal } from '../finui/components/modal/modal.js';
import { button } from '../finui/components/button/button.js';
import { formField } from '../finui/components/field/field.js';
import { t } from './i18n.js';

let open = null;

/** The dialog: true once the server took the password (and code), false when it was closed. One at a time. */
export function askPassword() {
  if (open) return open;
  open = new Promise((resolve) => {
    let answered = false;
    const done = (v) => { if (answered) return; answered = true; m.close(); resolve(v); };
    const sso = !!(state.user && state.user.sso);
    let body;
    if (sso) {
      const again = button({ variant: 'primary', onClick: async () => {
        try { await post('/api/logout'); } catch { /* signed out or not, the sign-in page is next */ }
        state.user = null;
        done(false);
        navigate('/login?next=' + encodeURIComponent(location.pathname + location.search), { replace: true });
      } }, t('reauth.sign_in_again'));
      body = h('div', { class: 'stack' }, h('p', null, t('reauth.sso')),
        h('div', { class: 'row row--end' }, button({ onClick: () => done(false) }, t('common.cancel')), again));
    } else {
      const pw = formField({ id: 'reauth-password', label: t('auth.password'), type: 'password', autocomplete: 'current-password' });
      const code = formField({ id: 'reauth-code', label: t('reauth.code'), inputMode: 'numeric', autocomplete: 'one-time-code', help: t('reauth.code_help') });
      code.el.hidden = true;
      const go = button({ variant: 'primary', type: 'submit' }, t('reauth.confirm'));
      const form = h('form', { class: 'stack', novalidate: true }, h('p', null, t('reauth.why')), pw.el, code.el,
        h('div', { class: 'row row--end' }, button({ onClick: () => done(false) }, t('common.cancel')), go));
      form.addEventListener('submit', async (e) => {
        e.preventDefault();
        pw.setError(''); code.setError('');
        go.disabled = true;
        try {
          await post('/api/me/confirm', { password: pw.input.value, code: code.input.value });
          done(true);
        } catch (err) {
          if (err.code === 'totp') { code.el.hidden = false; code.input.focus(); }
          else if (err.field === 'code') { code.el.hidden = false; code.setError(err.message); code.input.select(); }
          else { pw.setError(err.message); pw.input.select(); }
        } finally { go.disabled = false; }
      });
      body = form;
      setTimeout(() => pw.input.focus(), 0);
    }
    const m = openModal({ title: t('reauth.title'), body, onClose: () => { if (!answered) { answered = true; resolve(false); } } });
  }).finally(() => { open = null; });
  return open;
}

/** True when the session is recent for a while yet, else what the dialog answers. */
export async function ensureRecent() {
  try {
    const me = await get('/api/me');
    if (me && me.confirmed_until == null) return true;
    if (me && me.confirmed_until - Date.now() / 1000 > 60) return true;
  } catch { /* the request itself will say what is wrong */ return true; }
  return askPassword();
}
