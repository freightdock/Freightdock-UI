// Who you are signed in as, signing out, your password, two-factor, linking to single sign-on, your API tokens (for
// scripts, shown once when made), and the CA to trust for Freightdock's own certificate.

import { h, ago, when, num } from '../dom.js';
import { t, LANGUAGES, storedLanguage, pickLanguage, setLanguage } from '../i18n.js';
import { get, post, put, del } from '../api.js';
import { state } from '../state.js';
import { navigate } from '../router.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { callout } from '../../finui/components/callout/callout.js';
import { codeLine } from '../../finui/components/code-block/code-block.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { codeField, setupBox, showRecoveryCodes } from './two-factor.js';

const LIFETIMES = [{ value: '30', label: t('pages.container.range.days', { count: 30 }) }, { value: '90', label: t('pages.container.range.days', { count: 90 }) }, { value: '365', label: t('pages.account.a_year') }, { value: '', label: t('pages.account.until_revoked') }];

/** Goes to a provider's address the server passed on, only when it is http or https ("SSO" 16): it comes from the
 *  provider's discovery document, and a `javascript:` address would run in this page. Answers whether it went. */
function goToProvider(address) {
  let u;
  try { u = new URL(String(address), location.href); } catch { return false; }
  if (u.protocol !== 'https:' && u.protocol !== 'http:') return false;
  location.assign(u.href);
  return true;
}

/** The token, once: it is not shown again. */
function showToken(made) {
  openModal({ title: t('pages.account.your_token', { name: made.name }), wide: true, body: h('div', { class: 'stack' },
    callout({ tone: 'info', body: t('pages.account.copy_now') }),
    codeLine(made.token, { label: t('pages.account.copy_token') }),
    made.read_only ? h('p', { class: 'faint' }, t('pages.account.read_only_note')) : null) });
}

/** Where Freightdock's own certificate authority is downloaded: trusted once, it vouches for this server's certificate. */
export const CA_URL = '/api/tls/ca.pem';

/** Trusting Freightdock's CA: the download, and how a machine is told to trust it. With no CA (a certificate of one's
 *  own, or a proxy in front) there is nothing to trust here. */
function caHelp(hasCa) {
  if (!hasCa) return null;
  const how = (label, line) => h('div', { class: 'stack stack--tight' }, h('span', { class: 'faint' }, label), codeLine(line, { label: t('pages.account.copy_label', { label }) }));
  return h('div', { class: 'stack' },
    h('p', { class: 'faint' }, t('pages.account.ca_intro')),
    h('div', { class: 'row' }, button({ href: CA_URL, download: 'freightdock-ca.pem', size: 'sm' }, icon('download', 14), t('pages.account.download_ca'))),
    h('details', null, h('summary', null, t('pages.account.whole_machine')), h('div', { class: 'stack' },
      how('Debian, Ubuntu', 'sudo cp freightdock-ca.pem /usr/local/share/ca-certificates/freightdock-ca.crt && sudo update-ca-certificates'),
      how('Fedora, Arch', 'sudo trust anchor --store freightdock-ca.pem'),
      how('macOS', 'sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain freightdock-ca.pem'),
      how(t('pages.account.windows'), 'Import-Certificate -FilePath freightdock-ca.pem -CertStoreLocation Cert:\\LocalMachine\\Root'),
      how('curl', 'curl --cacert freightdock-ca.pem https://<host>/api/health'),
      h('p', { class: 'faint' }, t('pages.account.browsers')))));
}

/** Freightdock's own CA, when it made its certificate: `GET /api/tls/ca.pem` answers it, else 404. */
function caCard(signal) {
  const slot = h('div');
  loadInto(slot, { signal, load: (s) => fetch(CA_URL, { method: 'HEAD', signal: s }).then((r) => r.ok), draw: (has) => caHelp(has) || h('p', { class: 'faint' }, t('pages.account.no_ca')) });
  return card({ title: t('pages.account.certificate'), sub: t('pages.account.certificate_sub'), body: slot });
}

function newTokenDialog(after) {
  const name = formField({ id: 'token-name', label: t('common.name'), placeholder: 'backup-script', autocomplete: 'off', help: t('pages.account.token_name_help') });
  let days = '90';
  let readOnly = false;
  const lifetime = segmented({ label: t('pages.account.good_for'), value: days, options: LIFETIMES, onChange: (v) => { days = v; } });
  const roLabel = h('span', { id: 'token-ro-label' }, t('pages.account.read_only'));
  const roHelp = h('span', { id: 'token-ro-help', class: 'faint' }, t('pages.account.read_only_help'));
  const ro = toggle({ checked: false, labelledby: 'token-ro-label', describedby: 'token-ro-help', onChange: (v) => { readOnly = v; } });
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.account.make_token'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el,
    h('div', { class: 'stack' }, h('span', { class: 'faint' }, t('pages.account.good_for')), lifetime),
    h('div', { class: 'row' }, ro, h('div', { class: 'stack' }, roLabel, roHelp)),
    h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.account.new_token_title'), body: form, initialFocus: name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    name.setError('');
    go.disabled = true;
    try {
      const made = await post('/api/tokens', { name: name.input.value, expires_days: days ? Number(days) : null, read_only: readOnly });
      m.close();
      after();
      showToken(made);
    } catch (err) {
      name.setError(err.message);
      name.input.focus();
    } finally { go.disabled = false; }
  });
}

async function revoke(tok, after) {
  const ok = await confirm({ title: t('pages.account.revoke_title', { name: tok.name }), action: t('pages.account.revoke'), body: t('pages.account.revoke_body') });
  if (!ok) return;
  try { await del(`/api/tokens/${tok.id}`); toast({ text: t('pages.account.revoked'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

function tokensCard(signal) {
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get('/api/tokens', { signal: s }),
    draw: (list, again) => table({
      rows: list, filter: false, label: t('pages.account.api_tokens'), empty: [t('pages.account.no_tokens'), t('pages.account.no_tokens_text')],
      columns: [
        { label: t('common.name'), cell: (tok) => h('div', { class: 'name-cell' }, h('span', { class: 'truncate' }, tok.name), h('span', { class: 'sub mono' }, `fdk_…${tok.hint}`)) },
        { label: t('pages.account.may'), cell: (tok) => (tok.read_only ? status({ tone: 'info' }, t('pages.account.read_only')) : status({ tone: 'warning' }, t('pages.account.everything_you_may'))) },
        { label: t('pages.account.expires'), cell: (tok) => (tok.expires_at ? h('span', { class: 'nowrap', title: when(tok.expires_at) }, when(tok.expires_at)) : h('span', { class: 'faint' }, t('common.never'))), sort: (tok) => tok.expires_at || Infinity },
        { label: t('pages.account.last_used'), cell: (tok) => (tok.last_used_at ? h('span', { class: 'nowrap', title: tok.last_used_ip ? t('pages.account.used_from', { when: when(tok.last_used_at), ip: tok.last_used_ip }) : when(tok.last_used_at) }, ago(tok.last_used_at)) : h('span', { class: 'faint' }, t('common.never'))), sort: (tok) => tok.last_used_at || 0 },
        { label: '', nosort: true, cell: (tok) => h('div', { class: 'cell-actions' },
          button({ variant: 'icon', size: 'sm', 'aria-label': t('pages.account.revoke_label', { name: tok.name }), onClick: () => revoke(tok, again) }, icon('trash', 14))) },
      ],
    }),
  });
  const make = button({ size: 'sm', onClick: () => newTokenDialog(reload) }, icon('plus', 14), t('pages.account.new_token'));
  return card({ title: t('pages.account.api_tokens'), sub: t('pages.account.tokens_sub'), actions: make, body: slot });
}

/** One's own password, with the one it replaces; one's other sessions end, and one's API tokens are revoked. */
function passwordCard() {
  const current = formField({ id: 'pw-current', label: t('pages.account.your_password'), type: 'password', autocomplete: 'current-password' });
  const next = formField({ id: 'pw-new', label: t('pages.account.a_new_one'), type: 'password', autocomplete: 'new-password', help: t('pages.account.new_one_help') });
  const go = button({ type: 'submit' }, t('pages.account.change_it'));
  const form = h('form', { class: 'stack', novalidate: true }, current.el, next.el, h('div', { class: 'row row--end' }, go));
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    current.setError(''); next.setError('');
    go.disabled = true;
    try {
      await put('/api/me/password', { current: current.input.value, password: next.input.value });
      current.input.value = ''; next.input.value = '';
      toast({ text: t('pages.account.password_changed'), tone: 'good' });
    } catch (err) { const f = err.field === 'current' ? current : next; f.setError(err.message); f.input.focus(); } finally { go.disabled = false; }
  });
  return card({ title: t('auth.password'), body: form });
}

/** Linking this account to a sign-in provider, so it can be signed in to through it ("SSO" 7). An account is linked to
 *  one provider at most ("SSO" 13). */
function ssoCard(query) {
  const providers = (state.status && state.status.sso && state.status.sso.providers) || [];
  if (!providers.length) return null;
  const linked = query.get('sso') === 'linked', failed = query.get('sso_error');
  const buttons = providers.map((p) => {
    const go = button({ onClick: async () => {
      go.disabled = true;
      try { const r = await post(`/api/me/sso-link?provider=${encodeURIComponent(p.id)}`); if (!goToProvider(r.redirect)) throw new Error(t('pages.account.bad_redirect')); } catch (e) { toast({ text: e.message, tone: e.status === 409 ? 'info' : 'critical' }); go.disabled = false; }
    } }, icon('key', 14), t('pages.account.link_to', { name: p.label }));
    return go;
  });
  return card({ title: t('pages.account.sso'), sub: t('pages.account.sso_sub'), body: h('div', { class: 'stack' },
    linked ? callout({ tone: 'good', body: t('pages.account.linked') }) : null,
    failed ? callout({ tone: 'critical', body: failed }) : null,
    h('div', { class: 'row' }, ...buttons)) });
}

/** Asks for a code in a modal, then `act(code)`; an error about the code stays on it. */
function withCode({ title, action, body = null, recovery = true, act }) {
  const code = codeField('acct-code', { recovery });
  const go = button({ variant: 'primary', type: 'submit' }, action);
  const form = h('form', { class: 'stack', novalidate: true }, body, code.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title, body: form, initialFocus: code.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    code.setError('');
    go.disabled = true;
    try { await act(code.input.value); m.close(); } catch (err) { code.setError(err.message); code.input.select(); } finally { go.disabled = false; }
  });
}

/** Setting up an authenticator: the QR code and key, and the first code to confirm it; then the recovery codes. */
async function enrol(after) {
  let made;
  try { made = await post('/api/me/totp'); } catch (e) { toast({ text: e.message, tone: 'critical' }); return; }
  const code = codeField('enrol-code');
  const go = button({ variant: 'primary', type: 'submit' }, t('pages.account.turn_on'));
  const form = h('form', { class: 'stack', novalidate: true }, setupBox(made, code), h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.account.setup_title'), wide: true, body: form, initialFocus: code.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    code.setError('');
    go.disabled = true;
    try {
      const r = await post('/api/me/totp/confirm', { code: code.input.value });
      m.close();
      showRecoveryCodes(r.recovery_codes, { onClose: after });
    } catch (err) { code.setError(err.message); code.input.select(); } finally { go.disabled = false; }
  });
}

/** Two-factor for one's own sign-in with a password ("Two-factor" in DECISIONS.md). */
function twoFactorCard(signal) {
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get('/api/me/totp', { signal: s }),
    draw: (tok) => {
      if (!tok.password_signin) return h('p', { class: 'faint' }, t('pages.account.via_provider'));
      if (!tok.enabled) {
        return h('div', { class: 'stack' },
          tok.required ? callout({ tone: 'warning', body: t('pages.account.required_next') }) : null,
          h('p', null, t('pages.account.totp_intro')),
          h('div', null, button({ variant: 'primary', onClick: () => enrol(reload) }, icon('lock', 14), t('pages.account.setup_title'))));
      }
      return h('div', { class: 'stack' },
        facts([[t('pages.account.two_factor'), t('pages.account.on')], [t('pages.account.recovery_left'), num(tok.recovery_left)]]),
        tok.recovery_left < 3 ? callout({ tone: 'warning', body: t('pages.account.few_codes') }) : null,
        h('div', { class: 'row' },
          button({ onClick: () => withCode({ title: t('pages.account.new_codes'), action: t('pages.account.make_them'), recovery: false, body: h('p', null, t('pages.account.old_stop')),
            act: async (code) => { const r = await post('/api/me/totp/recovery', { code }); showRecoveryCodes(r.recovery_codes, { onClose: reload }); } }) }, t('pages.account.new_codes')),
          tok.required ? null : button({ variant: 'danger', onClick: () => withCode({ title: t('pages.account.turn_off_title'), action: t('pages.account.turn_off'), body: h('p', null, t('pages.account.password_alone')),
            act: async (code) => { await post('/api/me/totp/off', { code }); toast({ text: t('pages.account.totp_off'), tone: 'good' }); reload(); } }) }, t('pages.account.turn_off'))));
    },
  });
  return card({ title: t('pages.account.two_factor'), sub: t('pages.account.two_factor_sub'), body: slot });
}

/** The language this browser reads Freightdock in ("UI" 17): its own choice, or the browser's when it has none. Each
 *  language is named in its own words; choosing one reads the page again in it. */
function languageCard() {
  const browser = LANGUAGES.find((l) => l.tag === pickLanguage(navigator.languages || [navigator.language]));
  const chosen = storedLanguage();
  const options = [{ value: '', label: t('pages.account.language_browser', { name: browser.name }) }, ...LANGUAGES.map((l) => ({ value: l.tag, label: l.name }))];
  const select = h('select', { class: 'fui-field__input', id: 'language', lang: '' }, options.map((o) => h('option', { value: o.value, lang: o.value || null, selected: o.value === chosen }, o.label)));
  select.addEventListener('change', () => setLanguage(select.value));
  return card({ title: t('pages.account.language'), sub: t('pages.account.language_sub'), body: h('div', { class: 'fui-field' },
    h('label', { class: 'fui-field__label', for: 'language' }, t('pages.account.language')), select) });
}

export function accountPage({ el, signal, query }) {
  const me = state.user;
  const signin = me.signin || { kind: 'local', provider: null };
  if (new URLSearchParams(location.search).get('sso') === 'linked') toast({ text: signin.provider ? t('pages.account.linked_to', { name: signin.provider }) : t('pages.account.linked'), tone: 'good' });
  const out = button({ onClick: async () => {
    out.disabled = true;
    let r = null;
    try { r = await post('/api/logout'); } finally {
      state.user = null;
      // Signed in through the provider: signed out there too, so the next sign-in asks again.
      if (!(r && r.redirect && goToProvider(r.redirect))) navigate('/login', { replace: true });
    }
  } }, icon('logout', 14), t('pages.account.sign_out'));
  el.append(pageHeader(t('nav.account'), t('pages.account.sub')),
    h('div', { class: 'stack' },
      card({ title: me.name, body: h('div', { class: 'stack' }, facts([[t('pages.swarm.role'), h('span', { class: 'row' }, me.role === 'admin' ? t('common.administrator') : t('pages.account.reads_the_cluster'), signinBadge(signin))]]), h('div', null, out)) }),
      languageCard(),
      // A password is changed here only when it is kept here; two-factor guards a password, so a provider's sign-in has
      // neither (its provider gives both); linking is for a local account not linked yet ("Users" 2, 3).
      signin.kind === 'local' ? passwordCard() : null,
      me.sso ? null : twoFactorCard(signal),
      signin.kind === 'local' ? ssoCard(query) : null,
      tokensCard(signal),
      caCard(signal)));
}

/** How the account signs in, beside its role: the provider's name with a key, or the directory's ("Users" 3). Nothing
 *  for a local account. */
function signinBadge(signin) {
  if (signin.kind === 'local') return null;
  const ldap = signin.kind === 'ldap';
  const name = signin.provider || (ldap ? t('pages.account.a_directory') : t('pages.account.single_sign_on'));
  return h('span', { title: ldap ? t('pages.account.signs_in_ldap', { name }) : t('pages.account.signs_in_sso', { name }) }, status({ tone: 'info', icon: ldap ? 'server' : 'key', outlined: true }, ldap ? t('pages.account.badge_ldap', { name }) : t('pages.account.badge_sso', { name })));
}
