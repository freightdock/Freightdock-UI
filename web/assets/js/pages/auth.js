// The card setup and sign-in are drawn in, and the form both use: a name, a password (and for setup the setup token), a
// button, and the server's answer beside the field it is about.

import { h, brand } from '../dom.js';
import { formField } from '../../finui/components/field/field.js';
import { button } from '../../finui/components/button/button.js';
import { spinner } from '../../finui/components/spinner/spinner.js';
import { callout } from '../../finui/components/callout/callout.js';
import { t } from '../i18n.js';

export function authCard(title, sub, ...body) {
  return h('div', { class: 'auth-card' },
    h('div', { class: 'auth-brand' }, brand(32)),
    h('h1', { class: 'auth-title' }, title), sub ? h('p', { class: 'auth-sub' }, sub) : null, ...body);
}

/**
 * The name and password form. `submit({ name, password })` answers a promise; an ApiError shows beside its field. With
 * `tokenHelp`, a setup token field comes first and `submit` gets `{ token, name, password }`.
 */
export function credentialsForm({ action, passwordHelp = null, autocomplete = 'current-password', primary = true, tokenHelp = null, submit }) {
  const token = tokenHelp ? formField({ id: 'token', label: t('auth.setup_token'), autocomplete: 'off', help: tokenHelp }) : null;
  const name = formField({ id: 'name', label: t('auth.user_name'), autocomplete: 'username' });
  const password = formField({ id: 'password', label: t('auth.password'), type: 'password', autocomplete, help: passwordHelp });
  const general = h('div', { class: 'auth-general' });
  const go = button({ variant: primary ? 'primary' : 'default', type: 'submit', block: true }, action);
  const form = h('form', { class: 'auth-form', novalidate: true }, token?.el, name.el, password.el, general, go);
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    token?.setError(''); name.setError(''); password.setError(''); general.replaceChildren();
    go.disabled = true;
    go.replaceChildren(spinner(14), action);
    try {
      const c = { name: name.input.value, password: password.input.value };
      await submit(token ? { token: token.input.value.trim(), ...c } : c);
    } catch (err) {
      if (err.field === 'token' && token) token.setError(err.message);
      else if (err.field === 'name') name.setError(err.message);
      else if (err.field === 'password') password.setError(err.message);
      else general.replaceChildren(callout({ tone: 'critical', body: err.message }));
    } finally {
      go.disabled = false;
      go.replaceChildren(action);
    }
  });
  if (primary) queueMicrotask(() => (token ?? name).input.focus());
  return form;
}
