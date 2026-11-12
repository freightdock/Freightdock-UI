// The activity log: every change made through Freightdock, newest first, exported as CSV; sign-ins are the auth log's
// ("Activity" 3). Administrators only.

import { h } from '../dom.js';
import { get, qs } from '../api.js';
import { when, ago } from '../dom.js';
import { loadInto, table } from '../view.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { segmented } from '../../finui/components/segmented/segmented.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

const KINDS = [['', t('pages.events.everything')], ['container', t('nav.containers')], ['stack', t('nav.stacks')], ['user', t('grants.people')], ['access', t('pages.activity.roles')], ['image', t('nav.images')], ['volume', t('nav.volumes')], ['network', t('nav.networks')]];

function tone(action) {
  if (action.endsWith('.failed')) return 'critical';
  if (/(remove|delete|kill|prune)/.test(action)) return 'warning';
  return 'info';
}

export function activityPage({ el, signal }) {
  let kind = '';
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get('/api/audit' + qs({ action: kind, limit: 300 }), { signal: s }),
    draw: (d) => table({
      rows: d.entries, empty: [t('pages.activity.empty'), t('pages.activity.empty_text')], label: t('pages.activity.filter'),
      columns: [
        { label: t('pages.events.when'), cell: (e) => h('span', { title: when(e.at), class: 'nowrap' }, ago(e.at)), sort: (e) => e.at },
        { label: t('pages.activity.who'), cell: (e) => (e.user_name
          ? h('div', { class: 'name-cell' }, h('span', { class: 'truncate' }, e.user_name), e.via ? h('span', { class: 'sub' }, t('pages.activity.with', { via: e.via })) : null)
          : h('span', { class: 'faint' }, t('pages.registries.nobody'))) },
        { label: t('pages.events.what'), cell: (e) => status({ tone: tone(e.action) }, e.action) },
        { label: t('pages.events.which'), cell: (e) => h('span', { class: 'mono truncate', title: e.target || '' }, e.target || '') },
        { label: t('pages.events.detail'), cell: (e) => h('span', { class: 'truncate', title: e.detail || '' }, e.detail || '') },
        { label: t('pages.stacks.from'), cell: (e) => h('span', { class: 'mono faint' }, e.ip || '') },
      ],
    }),
  });
  const csv = button({ size: 'sm', onClick: () => location.assign('/api/audit' + qs({ action: kind, format: 'csv' })) }, icon('download', 13), t('pages.activity.csv'));
  el.append(pageHeader(t('nav.activity'), t('pages.activity.sub'), csv),
    h('div', { class: 'toolbar' }, segmented({ label: t('pages.events.show'), options: KINDS.map(([value, label]) => ({ value, label })), value: kind, onChange: (v) => { kind = v; reload(); } })),
    slot);
}
