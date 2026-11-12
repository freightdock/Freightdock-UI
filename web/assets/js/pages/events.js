// Events: everything Docker does, as it does it, from the last hour on.

import { h, when } from '../dom.js';
import { socket } from '../api.js';
import { base } from '../docker.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { button } from '../../finui/components/button/button.js';
import { status } from '../../finui/components/badge/badge.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

const KINDS = [['', t('pages.events.everything')], ['container', t('nav.containers')], ['image', t('nav.images')], ['volume', t('nav.volumes')], ['network', t('nav.networks')]];
const tone = (a) => (/die|kill|oom|destroy|delete|untag/.test(a) ? 'critical' : /stop|pause|unhealthy/.test(a) ? 'warning' : /start|create|pull|healthy/.test(a) ? 'good' : 'info');

export function eventsPage({ el, cleanup }) {
  let kind = '', paused = false, ws = null;
  const body = h('tbody');
  const tableEl = h('div', { class: 'fui-data-table__scroll' }, h('table', { class: 'fui-data-table fui-data-table--hover fui-data-table--dense' },
    h('thead', null, h('tr', null, [t('pages.events.when'), t('pages.networks.kind'), t('pages.events.what'), t('pages.events.which'), t('pages.events.detail')].map((label) => h('th', null, label)))), body));
  const connect = () => {
    if (ws) ws.close();
    body.replaceChildren();
    ws = socket(base(`/events/ws?since=${Math.floor(Date.now() / 1000) - 3600}${kind ? '&type=' + kind : ''}`));
    ws.onmessage = (e) => {
      if (paused) return;
      let ev; try { ev = JSON.parse(e.data); } catch { return; }
      if (!ev.Action) return;
      const a = ev.Actor || {}, at = a.Attributes || {};
      const name = at.name || (a.ID || '').slice(0, 19);
      const extra = Object.entries(at).filter(([k]) => !['name', 'image'].includes(k) && !k.startsWith('com.docker.compose') && !k.startsWith('org.opencontainers')).slice(0, 3).map(([k, v]) => `${k}=${v}`).join(' ');
      body.prepend(h('tr', null,
        h('td', { class: 'nowrap mono' }, when(ev.time)), h('td', null, ev.Type), h('td', null, status({ tone: tone(ev.Action) }, ev.Action)),
        h('td', null, ev.Type === 'container' ? h('a', { href: `/containers/${encodeURIComponent(name)}` }, name) : h('span', { class: 'mono' }, name)),
        h('td', { class: 'faint mono truncate' }, extra || at.image || '')));
      while (body.rows.length > 500) body.lastChild.remove();
    };
  };
  cleanup(() => ws && ws.close());
  const pause = button({ size: 'sm', onClick: () => { paused = !paused; pause.replaceChildren(icon(paused ? 'play' : 'pause', 13), paused ? t('pages.events.go_on') : t('pages.events.hold')); } }, icon('pause', 13), t('pages.events.hold'));
  el.append(pageHeader(t('nav.events'), t('pages.events.sub')),
    h('div', { class: 'toolbar' }, segmented({ label: t('pages.events.show'), size: 'sm', value: kind, options: KINDS.map(([value, label]) => ({ value, label })), onChange: (v) => { kind = v; connect(); } }), pause),
    tableEl);
  connect();
}
