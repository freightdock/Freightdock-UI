// Swarm stacks: Compose files deployed with docker stack deploy. The list (from the services' stack label, and the files
// Freightdock keeps), a new one written in the editor, and one stack: its services, its file to change and deploy again,
// and removing it.

import { h, ago, when, mount, num } from '../dom.js';
import { get, post, del } from '../api.js';
import { loadInto, table, filterBox, matches } from '../view.js';
import { can } from '../state.js';
import { navigate } from '../router.js';
import { confirm } from '../confirm.js';
import { sbase, swarmPage, replicas, serviceImage, servicePorts } from '../swarm.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { card } from '../../finui/components/card/card.js';
import { button } from '../../finui/components/button/button.js';
import { formField } from '../../finui/components/field/field.js';
import { callout } from '../../finui/components/callout/callout.js';
import { status } from '../../finui/components/badge/badge.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { codeEditor } from '../components/code-editor/code-editor.js';
import { t } from '../i18n.js';

const EXAMPLE = `services:
  web:
    image: nginx:alpine
    ports:
      - "8080:80"
    deploy:
      replicas: 2
      update_config:
        parallelism: 1
        order: start-first
`;

const count = (s) => status({ tone: s.running === s.desired ? 'good' : s.running === 0 ? 'critical' : 'warning', line: true, class: 'nowrap' }, `${s.running}/${s.desired}`);

export const stacksPage = swarmPage(({ el, signal, every }) => {
  let words = [];
  const slot = h('div');
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(sbase('/stacks'), { signal: s }),
    draw: (list) => table({
      rows: list.filter((s) => matches(words, s.name)), filter: false, empty: [t('pages.stacks.empty'), t('pages.swarm_stacks.empty_text')],
      columns: [
        { label: t('common.name'), cell: (s) => h('div', { class: 'name-cell' }, h('a', { href: `/swarm/stacks/${encodeURIComponent(s.name)}` }, s.name), s.file ? null : h('span', { class: 'sub' }, t('pages.swarm_stacks.elsewhere_no_file'))) },
        { label: t('nav.services'), cls: 'r', cell: (s) => num(s.services), sort: (s) => s.services },
        { label: t('pages.swarm.tasks'), cell: (s) => (s.services ? count(s) : status({ tone: 'info', line: true }, t('pages.stacks.state.down'))), sort: (s) => s.running },
        { label: t('pages.stacks.deployed'), cell: (s) => (s.updated_at ? h('span', { class: 'nowrap' }, ago(s.updated_at)) : ''), sort: (s) => s.updated_at || 0 },
      ],
    }),
  });
  every(10000, reload);
  const actions = can('administer') ? button({ size: 'sm', variant: 'primary', href: '/swarm/stacks/new' }, icon('plus', 14), t('routes.deploy_stack')) : null;
  el.append(pageHeader(t('nav.swarm_stacks'), t('pages.swarm_stacks.sub'), actions),
    h('div', { class: 'toolbar' }, filterBox(t('pages.stacks.filter'), (w) => { words = w; reload.redraw(); })), slot);
});

/** The editor and its Deploy button; `name` given for a stack that is there already. */
function deployForm({ name = null, compose = EXAMPLE, after }) {
  const nameField = formField({ id: 'stack-name', label: t('common.name'), autocomplete: 'off', help: t('pages.swarm_stacks.name_help') });
  const editor = codeEditor({ id: 'stack-compose', label: t('pages.stacks.compose_file'), language: 'yaml', value: compose, rows: 18 });
  const err = h('div');
  const go = button({ variant: 'primary', type: 'submit' }, icon('rocket', 14), name ? t('pages.swarm_stacks.deploy_again') : t('pages.stack.deploy'));
  const form = h('form', { class: 'stack', novalidate: true },
    name ? null : nameField.el,
    h('div', { class: 'fui-field' }, h('label', { class: 'fui-field__label', htmlFor: 'stack-compose' }, t('pages.stacks.compose_file')), editor.el,
      h('p', { class: 'fui-field__help' }, t('pages.swarm_stacks.compose_help'))),
    err, h('div', { class: 'row row--end' }, go));
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); nameField.setError(''); mount(err);
    const n = name || nameField.input.value.trim();
    go.disabled = true;
    try {
      const r = await post(sbase('/stacks'), { name: n, compose: editor.getValue() });
      toast({ text: t('pages.swarm_stacks.deployed', { name: n }), tone: 'good' });
      after(n, r);
    } catch (x) {
      if (x.field === 'name') nameField.setError(x.message);
      else mount(err, callout({ tone: 'critical', title: t('pages.swarm_stacks.not_deployed'), body: h('pre', { class: 'pre mono' }, x.message) }));
    } finally { go.disabled = false; }
  });
  return form;
}

export const newStackPage = swarmPage(({ el }) => {
  el.append(pageHeader(t('routes.deploy_stack'), t('pages.swarm_stacks.new_sub')),
    deployForm({ after: (n) => navigate(`/swarm/stacks/${encodeURIComponent(n)}`) }));
});

export const stackPage = swarmPage(({ el, params, signal }) => {
  const slot = h('div');
  el.append(slot);
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(sbase(`/stacks/${encodeURIComponent(params.name)}`), { signal: s }),
    draw: (st) => {
      const actions = can('administer') ? button({ size: 'sm', variant: 'danger', onClick: async () => {
        if (!(await confirm({ title: t('common.remove_name', { name: st.name }), body: t('pages.swarm_stacks.remove_body') }))) return;
        try { await del(sbase(`/stacks/${encodeURIComponent(st.name)}`)); toast({ text: t('common.removed'), tone: 'good' }); navigate('/swarm/stacks'); } catch (e) { toast({ text: e.message, tone: 'critical' }); }
      } }, icon('trash', 13), t('common.remove')) : null;
      const services = card({ title: t('nav.services'), body: table({ rows: st.services, dense: true, filter: false, empty: [t('pages.stack.no_services'), t('pages.swarm_stacks.not_deployed_now')], columns: [
        { label: t('pages.stack.service'), cell: (s) => h('a', { href: `/swarm/services/${encodeURIComponent(s.Spec.Name)}` }, s.Spec.Name) },
        { label: t('pages.swarm.tasks'), cell: (s) => replicas(s) },
        { label: t('pages.containers.image'), cell: (s) => h('span', { class: 'mono truncate' }, serviceImage(s)) },
        { label: t('pages.containers.ports'), cell: (s) => h('span', { class: 'mono' }, servicePorts(s)) },
      ] }) });
      const sub = st.updated_at ? (st.updated_by ? t('pages.swarm_stacks.deployed_by', { when: when(st.updated_at), name: st.updated_by }) : t('pages.swarm_stacks.deployed_at', { when: when(st.updated_at) })) : t('pages.swarm_stacks.deployed_elsewhere');
      const file = st.compose == null
        ? callout({ tone: 'info', body: t('pages.swarm_stacks.no_file') })
        : null;
      const editor = can('administer')
        ? card({ title: t('pages.swarm_stacks.change_deploy'), body: deployForm({ name: st.name, compose: st.compose || '', after: () => reload() }) })
        : st.compose != null ? card({ title: t('pages.stacks.compose_file'), body: h('pre', { class: 'pre mono' }, st.compose) }) : null;
      return h('div', { class: 'stack' }, pageHeader(st.name, sub, actions), services, file, editor);
    },
  });
});
