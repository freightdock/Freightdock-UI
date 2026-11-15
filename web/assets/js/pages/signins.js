// The auth log ("Activity" 3): who came in, how and from where. Sign-ins and failed ones, sign-outs, SSO sign-ins and
// those turned away, accounts linked to the provider, API tokens' use; filtered, and exported as CSV. Administrators only.

import { h, when, ago } from '../dom.js';
import { get, qs } from '../api.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { formField } from '../../finui/components/field/field.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

const KINDS = [['', t('pages.events.everything')], ['login', t('pages.signins.sign_ins')], ['logout', t('pages.signins.sign_outs')], ['sso', t('pages.signins.linked')], ['token', t('pages.signins.tokens')]];
const OUTCOMES = [['', t('pages.signins.any')], ['true', t('pages.signins.let_in')], ['false', t('pages.signins.refused')]];
const SPANS = [['', t('pages.signins.any_time')], ['86400', t('pages.signins.a_day')], ['604800', t('pages.signins.a_week')], ['2592000', t('pages.signins.thirty_days')]];
const WORDS = { login: t('pages.signins.words.login'), 'login.failed': t('pages.signins.words.login_failed'), logout: t('pages.signins.words.logout'), 'login.sso': t('pages.signins.words.login_sso'), 'login.sso.failed': t('pages.signins.words.login_sso_failed'), 'sso.link': t('pages.signins.words.sso_link'), 'token.use': t('pages.signins.words.token_use'), confirm: t('pages.signins.words.confirm'), 'confirm.failed': t('pages.signins.words.confirm_failed') };

export function signinsPage({ el, signal }) {
  const f = { event: '', ok: '', span: '', user: '' };
  const filters = () => ({ event: f.event, ok: f.ok, user: f.user.trim(), since: f.span ? Math.floor(Date.now() / 1000) - Number(f.span) : '' });
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get('/api/auth-log' + qs({ ...filters(), limit: 500 }), { signal: s }),
    draw: (d) => table({
      rows: d.entries, filter: false, empty: [t('pages.signins.nothing'), t('pages.signins.nothing_text')],
      columns: [
        { label: t('pages.events.when'), cell: (e) => h('span', { title: when(e.at), class: 'nowrap' }, ago(e.at)), sort: (e) => e.at },
        { label: t('pages.activity.who'), cell: (e) => h('span', { class: 'truncate' }, e.user_name || t('docker.state.unknown')) },
        { label: t('pages.events.what'), cell: (e) => status({ tone: e.ok ? (e.event === 'token.use' ? 'info' : 'good') : 'critical' }, WORDS[e.event] || e.event) },
        { label: t('pages.signins.how'), cell: (e) => e.method },
        { label: t('pages.stacks.from'), cell: (e) => h('div', { class: 'name-cell' }, h('span', { class: 'mono' }, e.ip || ''), e.user_agent ? h('span', { class: 'sub truncate', title: e.user_agent }, e.user_agent) : null) },
        { label: t('pages.events.detail'), cell: (e) => h('span', { class: 'truncate', title: e.detail || '' }, e.detail || '') },
      ],
    }),
  });
  const who = formField({ id: 'auth-user', label: t('pages.signins.person'), placeholder: t('pages.signins.any') });
  who.input.addEventListener('change', () => { f.user = who.input.value; reload(); });
  const csv = button({ size: 'sm', onClick: () => location.assign('/api/auth-log' + qs({ ...filters(), format: 'csv' })) }, icon('download', 13), t('pages.activity.csv'));
  const seg = (label, key, options) => segmented({ label, size: 'sm', value: f[key], options: options.map(([value, l]) => ({ value, label: l })), onChange: (v) => { f[key] = v; reload(); } });
  el.append(pageHeader(t('nav.auth_log'), t('pages.signins.sub'), csv),
    h('div', { class: 'toolbar' }, seg(t('pages.networks.kind'), 'event', KINDS), seg(t('pages.signins.outcome'), 'ok', OUTCOMES), seg(t('pages.swarm.since'), 'span', SPANS), who.el),
    slot);
}
