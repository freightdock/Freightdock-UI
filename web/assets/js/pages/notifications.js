// Notifications, for administrators: the channels Freightdock tells things through (a webhook, e-mail, an ntfy topic, a
// Gotify server), what each is told about, a test message through each, and when each last worked or why it did not.

import { h, ago } from '../dom.js';
import { get, post, put, del } from '../api.js';
import { loadInto, table } from '../view.js';
import { confirm } from '../confirm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { formField } from '../../finui/components/field/field.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { choiceGroup } from '../../finui/components/choice/choice.js';
import { settingRow } from '../../finui/components/setting-row/setting-row.js';
import { toggle } from '../../finui/components/toggle/toggle.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t, tOr } from '../i18n.js';

const KINDS = [{ value: 'webhook', label: t('webhook.title') }, { value: 'email', label: t('pages.signin_settings.email') }, { value: 'ntfy', label: 'ntfy' }, { value: 'gotify', label: 'Gotify' }];
const KIND_NAME = Object.fromEntries(KINDS.map((k) => [k.value, k.label]));
/** An event's words: the catalogue's for the events it knows, else the server's. */
const eventLabel = (e) => tOr(`pages.notifications.events.${e.key.replace('.', '_')}`, e.label);

/** Each kind's fields: [key, label, options]. `secret` is the one the server keeps sealed. */
const FIELDS = {
  webhook: [['url', t('pages.environments.address'), { placeholder: 'https://hooks.example.com/freightdock', help: t('pages.notifications.webhook_help') }],
    ['secret', t('pages.notifications.signing_key'), { type: 'password', help: t('pages.notifications.signing_help') }]],
  ntfy: [['server', t('pages.notifications.server'), { placeholder: 'https://ntfy.sh' }], ['topic', t('pages.notifications.topic'), { placeholder: 'freightdock' }],
    ['priority', t('pages.notifications.priority'), { type: 'number', placeholder: '4', help: t('pages.notifications.ntfy_priority_help') }], ['secret', t('pages.notifications.access_token'), { type: 'password', help: t('pages.notifications.ntfy_token_help') }]],
  gotify: [['server', t('pages.notifications.server'), { placeholder: 'https://gotify.example.com' }], ['secret', t('pages.notifications.app_token'), { type: 'password' }],
    ['priority', t('pages.notifications.priority'), { type: 'number', placeholder: '5', help: t('pages.notifications.gotify_priority_help') }]],
  email: [['host', t('pages.notifications.smtp'), { placeholder: 'smtp.example.com' }], ['port', t('pages.notifications.port'), { type: 'number', placeholder: t('pages.notifications.port_placeholder') }],
    ['username', t('auth.user_name'), { autocomplete: 'off' }], ['secret', t('auth.password'), { type: 'password', autocomplete: 'new-password' }],
    ['from', t('pages.notifications.from'), { placeholder: 'Freightdock <freightdock@example.com>' }], ['to', t('pages.notifications.to'), { placeholder: 'ops@example.com, me@example.com', help: t('pages.notifications.comma_between') }]],
};

function channelDialog(meta, ch, after) {
  let kind = ch ? ch.kind : 'webhook', enabled = ch ? ch.enabled : true;
  let events = ch ? [...ch.events] : meta.events.map((e) => e.key);
  let security = (ch && ch.config.security) || 'starttls';
  const name = formField({ id: 'ch-name', label: t('common.name'), placeholder: 'ops', autocomplete: 'off' });
  name.input.value = ch ? ch.name : '';
  const fields = {};
  const slot = h('div', { class: 'stack' });
  const paint = () => {
    for (const k of Object.keys(fields)) delete fields[k];
    const rows = FIELDS[kind].map(([key, label, o]) => {
      const f = formField({ id: `ch-${key}`, label, type: o.type || 'text', placeholder: key === 'secret' && ch && ch.has_secret ? t('pages.registries.kept') : o.placeholder, help: o.help, autocomplete: o.autocomplete || 'off' });
      const v = ch && ch.kind === kind ? ch.config[key] : null;
      f.input.value = key === 'secret' ? '' : Array.isArray(v) ? v.join(', ') : v == null ? '' : String(v);
      fields[key] = f;
      return f.el;
    });
    const sec = kind === 'email' ? segmented({ label: t('pages.notifications.security'), size: 'sm', value: security, options: [{ value: 'starttls', label: 'STARTTLS' }, { value: 'tls', label: 'TLS' }, { value: 'none', label: t('pages.notifications.none') }], onChange: (v) => { security = v; } }) : null;
    slot.replaceChildren(...rows, ...(sec ? [h('div', { class: 'row' }, h('span', { class: 'muted' }, t('pages.notifications.security')), sec)] : []));
  };
  paint();
  const kindPick = ch ? h('p', { class: 'muted' }, KIND_NAME[kind]) : segmented({ label: t('pages.networks.kind'), size: 'sm', value: kind, options: KINDS, onChange: (v) => { kind = v; paint(); } });
  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  const go = button({ variant: 'primary', type: 'submit' }, ch ? t('common.save') : t('common.add'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, kindPick, slot,
    choiceGroup({ label: t('pages.notifications.tell_when'), kind: 'checkbox', options: meta.events.map((e) => ({ value: e.key, label: eventLabel(e) })), value: events, onChange: (v) => { events = v; } }),
    settingRow({ id: 'ch-on', label: t('pages.account.on'), help: t('pages.notifications.on_help'), control: toggle({ checked: enabled, labelledby: 'ch-on-label', onChange: (v) => { enabled = v; } }) }),
    err, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: ch ? t('routes.edit', { name: ch.name }) : t('pages.notifications.add'), body: form, initialFocus: name.input, wide: true });
  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    name.setError(''); err.replaceChildren();
    for (const f of Object.values(fields)) f.setError('');
    const config = { security };
    for (const [key, f] of Object.entries(fields)) {
      const v = f.input.value.trim();
      config[key] = key === 'to' ? v.split(',').map((x) => x.trim()).filter(Boolean) : key === 'port' || key === 'priority' ? (v ? Number(v) : null) : v;
    }
    go.disabled = true;
    try {
      const body = { name: name.input.value, kind, enabled, events, config };
      if (ch) await put(`/api/notifications/${ch.id}`, body); else await post('/api/notifications', body);
      m.close(); toast({ text: ch ? t('common.saved') : t('pages.notifications.added'), tone: 'good' }); after();
    } catch (x) {
      const f = x.field === 'name' ? name : fields[x.field];
      if (f) { f.setError(x.message); f.input.focus(); } else err.replaceChildren(x.message);
    } finally { go.disabled = false; }
  });
}

async function test(ch, after) {
  toast({ text: t('pages.notifications.sending', { name: ch.name }) });
  try { await post(`/api/notifications/${ch.id}/test`); toast({ text: t('pages.notifications.sent', { name: ch.name }), tone: 'good' }); }
  catch (e) { toast({ text: t('common.name_said', { name: ch.name, message: e.message }), tone: 'critical' }); }
  after();
}

async function remove(ch, after) {
  if (!(await confirm({ title: t('common.remove_name', { name: ch.name }), body: t('pages.notifications.remove_body') }))) return;
  try { await del(`/api/notifications/${ch.id}`); toast({ text: t('common.removed'), tone: 'good' }); after(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
}

function health(ch) {
  if (!ch.enabled) return status({ tone: 'info', line: true }, t('pages.notifications.off'));
  if (ch.last_error) return h('span', { class: 'stack-tight' }, status({ tone: 'critical', line: true }, t('pages.notifications.failing')), h('span', { class: 'sub' }, ch.last_error));
  if (ch.last_sent_at) return status({ tone: 'good', line: true, class: 'nowrap' }, t('pages.notifications.sent_when', { when: ago(ch.last_sent_at) }));
  return status({ tone: 'info', line: true }, t('pages.notifications.nothing_sent'));
}

export function notificationsPage({ el, signal }) {
  const slot = h('div');
  let meta = null;
  const reload = loadInto(slot, {
    signal,
    load: async (s) => { const r = await get('/api/notifications', { signal: s }); meta = r; return r; },
    draw: (r, again) => {
      const label = Object.fromEntries(r.events.map((e) => [e.key, eventLabel(e)]));
      return table({
        rows: r.channels, filter: false, empty: [t('pages.notifications.empty'), t('pages.notifications.empty_text')],
        columns: [
          { label: t('common.name'), cell: (c) => h('div', { class: 'name-cell' }, h('button', { type: 'button', class: 'link-button', onClick: () => channelDialog(meta, c, again) }, c.name), h('span', { class: 'sub' }, KIND_NAME[c.kind] || c.kind)) },
          { label: t('pages.notifications.told_about'), cell: (c) => (c.events.length ? h('span', { class: 'row' }, c.events.map((e) => h('span', { class: 'fui-chip', title: label[e] || e }, e))) : h('span', { class: 'faint' }, t('pages.notifications.tests_only'))) },
          { label: t('pages.notifications.last'), cell: health },
          { label: '', nosort: true, cell: (c) => h('div', { class: 'cell-actions' },
            button({ size: 'sm', onClick: () => test(c, again) }, icon('send', 13), t('pages.notifications.test')),
            button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: c.name }), onClick: () => remove(c, again) }, icon('trash', 14))) },
        ],
      });
    },
  });
  const actions = button({ size: 'sm', variant: 'primary', onClick: () => meta && channelDialog(meta, null, reload) }, icon('plus', 14), t('pages.notifications.add'));
  el.append(pageHeader(t('nav.notifications'), t('pages.notifications.sub'), actions), slot);
}
