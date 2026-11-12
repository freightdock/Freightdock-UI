// Swarm secrets and configs: each listed with the services that use it, made from a name and a value, looked at, and
// removed. A secret's value is written once and never shown again: Docker does not give it back, nor does Freightdock.
// A config is not secret, so its page shows its text.

import { h, ago, when, mount } from '../dom.js';
import { get, post, del } from '../api.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { confirm } from '../confirm.js';
import { sbase, swarmPage } from '../swarm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { openDrawer } from '../../finui/components/drawer/drawer.js';
import { formField, textArea } from '../../finui/components/field/field.js';
import { facts } from '../../finui/components/facts/facts.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

const KINDS = {
  secrets: { title: t('nav.secrets'), sub: t('pages.swarm_objects.secrets.sub'), valueHelp: t('pages.swarm_objects.secrets.value_help'), create: t('pages.swarm_objects.secrets.create'), createTitle: t('pages.swarm_objects.secrets.create_title'), empty: t('pages.swarm_objects.secrets.empty'), filter: t('pages.swarm_objects.secrets.filter') },
  configs: { title: t('nav.configs'), sub: t('pages.swarm_objects.configs.sub'), valueHelp: t('pages.swarm_objects.configs.value_help'), create: t('pages.swarm_objects.configs.create'), createTitle: t('pages.swarm_objects.configs.create_title'), empty: t('pages.swarm_objects.configs.empty'), filter: t('pages.swarm_objects.configs.filter') },
};

const toLabels = (s) => Object.fromEntries(s.split('\n').map((l) => l.trim()).filter(Boolean).map((l) => { const i = l.indexOf('='); return i < 0 ? [l, ''] : [l.slice(0, i).trim(), l.slice(i + 1).trim()]; }));
const labelText = (l) => Object.entries(l || {}).map(([k, v]) => (v ? `${k}=${v}` : k)).join('\n');

function createDialog(kind, after) {
  const k = KINDS[kind];
  const name = formField({ id: 'obj-name', label: t('common.name'), autocomplete: 'off' });
  const value = textArea({ id: 'obj-value', label: kind === 'secrets' ? t('pages.stack.value') : t('pages.swarm_objects.text'), rows: 8, help: k.valueHelp });
  value.input.classList.add('mono');
  value.input.setAttribute('autocomplete', 'off');
  value.input.setAttribute('spellcheck', 'false');
  const labels = textArea({ id: 'obj-labels', label: t('common.labels'), rows: 2, help: t('pages.container_edit.labels_help') });
  const err = h('p', { class: 'fui-field__error', role: 'alert' });
  const go = button({ variant: 'primary', type: 'submit' }, t('common.create'));
  const form = h('form', { class: 'stack', novalidate: true }, name.el, value.el, labels.el, err, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: k.createTitle, body: form, initialFocus: name.input });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); name.setError(''); mount(err);
    go.disabled = true;
    try {
      await post(sbase(`/${kind}`), { name: name.input.value.trim(), data: value.input.value, labels: toLabels(labels.input.value) });
      value.input.value = '';
      m.close(); toast({ text: t('common.created'), tone: 'good' }); after();
    } catch (x) { if (x.field === 'name') name.setError(x.message); else mount(err, x.message); } finally { go.disabled = false; }
  });
}

async function details(kind, o) {
  const d = await get(sbase(`/${kind}/${encodeURIComponent(o.ID)}`));
  openDrawer({ title: d.Spec.Name, body: h('div', { class: 'stack' },
    facts([[t('common.made'), when(Date.parse(d.CreatedAt) / 1000)], [t('pages.swarm_services.updated'), when(Date.parse(d.UpdatedAt) / 1000)], [t('pages.volumes.used_by'), (d.services || []).join(', ') || t('pages.swarm_objects.no_service'), { wide: true }],
      [t('common.labels'), labelText(d.Spec.Labels) || t('common.none_word'), { wide: true, mono: true }], ['Id', d.ID, { wide: true, mono: true }]]),
    kind === 'secrets'
      ? h('p', { class: 'muted' }, t('pages.swarm_objects.secret_hidden'))
      : [h('h3', { class: 'drawer-sub' }, t('pages.swarm_objects.text')), d.text == null ? h('p', { class: 'muted' }, t('pages.swarm_objects.not_text')) : h('pre', { class: 'pre mono' }, d.text)]) });
}

function objectsPage(kind) {
  const k = KINDS[kind];
  return swarmPage(({ el, signal }) => {
    let words = [];
    const slot = h('div');
    const reload = loadInto(slot, {
      signal,
      load: (s) => get(sbase(`/${kind}`), { signal: s }),
      draw: (list, again) => table({
        rows: list.filter((o) => matches(words, o.Spec.Name, ...(o.services || []))), filter: false, empty: [k.empty, ''],
        columns: [
          { label: t('common.name'), cell: (o) => h('button', { type: 'button', class: 'link-button', onClick: () => details(kind, o).catch((e) => toast({ text: e.message, tone: 'critical' })) }, o.Spec.Name) },
          { label: t('pages.volumes.used_by'), cell: (o) => ((o.services || []).length ? h('span', { class: 'row' }, o.services.map((s) => h('a', { class: 'fui-chip', href: `/swarm/services/${encodeURIComponent(s)}` }, s))) : status({ tone: 'warning', line: true }, t('pages.volumes.unused'))), sort: (o) => (o.services || []).length },
          { label: t('common.made'), cell: (o) => h('span', { class: 'nowrap' }, ago(Date.parse(o.CreatedAt) / 1000)), sort: (o) => Date.parse(o.CreatedAt) },
          { label: '', nosort: true, cell: (o) => (can('administer') ? h('div', { class: 'cell-actions' }, button({ variant: 'icon', size: 'sm', 'aria-label': t('common.remove_label', { name: o.Spec.Name }), onClick: async () => {
            const used = (o.services || []).length;
            if (!(await confirm({ title: t('common.remove_name', { name: o.Spec.Name }), body: used ? t('pages.swarm_objects.remove_used', { count: used }) : t('pages.swarm_objects.remove_unused') }))) return;
            try { await del(sbase(`/${kind}/${encodeURIComponent(o.ID)}`)); toast({ text: t('common.removed'), tone: 'good' }); again(); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
          } }, icon('trash', 14))) : '') },
        ],
      }),
    });
    const actions = can('administer') ? button({ size: 'sm', variant: 'primary', onClick: () => createDialog(kind, reload) }, icon('plus', 14), k.create) : null;
    el.append(pageHeader(k.title, k.sub, actions), h('div', { class: 'toolbar' }, filterBox(k.filter, (w) => { words = w; reload.redraw(); })), slot);
  });
}

export const secretsPage = objectsPage('secrets');
export const configsPage = objectsPage('configs');
