// The webhook card on a stack's and a container's page: whether it has one, when it was last called, and making a new
// address (shown this once: Freightdock keeps only its hash) or removing it. A CI job POSTs to the address to redeploy.

import { h, ago, mount } from './dom.js';
import { get, post, del } from './api.js';
import { base } from './docker.js';
import { can } from './state.js';
import { confirm } from './confirm.js';
import { card } from '../finui/components/card/card.js';
import { button } from '../finui/components/button/button.js';
import { callout } from '../finui/components/callout/callout.js';
import { codeLine } from '../finui/components/code-block/code-block.js';
import { toast } from '../finui/components/toast/toast.js';
import { t } from './i18n.js';

/** webhookCard({ kind: 'stack'|'container', target, path, what }): `target` as the server keeps it (a stack's id, a
 *  container's name), `path` the API path under the environment (`/stacks/3`), `what` the sentence that says what a POST
 *  to its address does. */
export function webhookCard({ kind, target, path, what }) {
  if (!can('manage')) return null;
  const body = h('div', { class: 'stack' });
  let shown = null;   // the address made in this visit, the one time it can be seen
  const draw = async () => {
    let hook = null;
    try { hook = (await get(base('/webhooks'))).find((w) => w.kind === kind && w.target === String(target)) || null; } catch (e) { mount(body, callout({ tone: 'critical', body: e.message })); return; }
    const make = button({ size: 'sm', variant: hook ? 'default' : 'primary', onClick: async () => {
      if (hook && !(await confirm({ title: t('webhook.new_address_title'), danger: false, action: t('webhook.make_it'), body: t('webhook.new_address_body') }))) return;
      try { shown = `${location.origin}${(await post(base(`${path}/webhook`))).url}`; draw(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
    } }, hook ? t('webhook.new_address') : t('webhook.make'));
    const remove = hook ? button({ size: 'sm', variant: 'ghost', onClick: async () => {
      if (!(await confirm({ title: t('webhook.remove_title'), body: t('webhook.remove_body') }))) return;
      try { await del(base(`${path}/webhook`)); shown = null; draw(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
    } }, t('common.remove')) : null;
    mount(body,
      h('p', { class: 'muted' }, `${what} ${t('webhook.no_sign_in')}`),
      shown ? h('div', { class: 'stack' }, callout({ tone: 'warning', body: t('webhook.copy_now') }),
        codeLine(shown, { label: t('webhook.copy_address') }), codeLine(`curl -fsS -X POST ${shown}`, { label: t('webhook.copy_command') }))
        : hook ? h('p', null, t(`webhook.there_is_one${hook.created_by ? '_by' : ''}${hook.last_used_at ? '' : '_never'}`, { by: hook.created_by || '', when: hook.last_used_at ? ago(hook.last_used_at) : '' })) : null,
      h('div', { class: 'row' }, make, remove));
  };
  draw();
  return card({ title: t('webhook.title'), sub: t('webhook.for_ci'), body });
}
