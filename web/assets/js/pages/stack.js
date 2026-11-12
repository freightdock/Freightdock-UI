// One stack: its state and its actions (deploy, pull and redeploy, stop, start, remove) with Compose's output as it
// comes, its containers, and its compose file and variables to edit and deploy again.

import { accessCard } from '../ownership.js';
import { h, ago, when, mount } from '../dom.js';
import { get, put } from '../api.js';
import { base, containerName, stateBadge, healthOf, portLinks } from '../docker.js';
import { loadInto, table } from '../view.js';
import { can } from '../state.js';
import { navigate } from '../router.js';
import { confirm } from '../confirm.js';
import { stackState, stackFiles, showError, runJob } from './stacks.js';
import { gitCard } from './stack-git.js';
import { codeEditor, ENV_HELP } from '../editor.js';
import { webhookCard } from '../webhook.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { button } from '../../finui/components/button/button.js';
import { dropdownMenu } from '../../finui/components/dropdown-menu/dropdown-menu.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { tabs } from '../../finui/components/tabs/tabs.js';
import { callout } from '../../finui/components/callout/callout.js';
import { toast } from '../../finui/components/toast/toast.js';
import { icon } from '../../finui/core.js';
import { t } from '../i18n.js';

function containersTable(list) {
  return table({ rows: list, filter: false, empty: [t('pages.containers.empty'), t('pages.stack.deploy_to_make')], columns: [
    { label: t('common.name'), cell: (c) => h('a', { href: `/containers/${encodeURIComponent(containerName(c))}` }, containerName(c)) },
    { label: t('pages.stack.service'), cell: (c) => (c.Labels || {})['com.docker.compose.service'] || '' },
    { label: t('common.state'), cell: (c) => h('div', { class: 'name-cell' }, stateBadge(c.State, healthOf(c.Status)), h('span', { class: 'sub' }, c.Status)), sort: (c) => c.State },
    { label: t('pages.containers.image'), cell: (c) => h('span', { class: 'mono truncate', title: c.Image }, c.Image) },
    { label: t('pages.containers.ports'), cell: (c) => portLinks(c.Ports), nosort: true },
  ] });
}

export function stackPage({ el, params, signal, every, query }) {
  const id = params.id;
  const slot = h('div');
  const out = h('div');
  let tab = query.get('tab') || 'overview';
  let busy = false;
  // The compose file's field, where a refusal by the host's security settings is shown ("Access" 16).
  let fileField = null;
  // Outside what refreshes: a new webhook address is shown once, and a refresh must not take it away.
  const hook = webhookCard({ kind: 'stack', target: id, path: `/stacks/${id}`, what: t('pages.stack.webhook_what') });
  el.append(slot, h('div', { class: 'stack-hook' }, hook), out);
  const job = async (word, body, after) => {
    if (busy) return;
    busy = true;
    const onError = (e) => { if (e.field === 'compose' && fileField) fileField.setError(e.message); };
    try { if (await runJob(id, word, body, out, { onError })) { if (after) after(); else reload(); } } finally { busy = false; }
  };
  // The Environment tab asks Compose once, when it is first opened (and again on Refresh), not on every redraw.
  const envPanel = h('div', { class: 'stack' });
  let envLoaded = false;
  const loadEnv = () => {
    envLoaded = true;
    loadInto(envPanel, { signal, load: (s) => get(base(`/stacks/${id}/environment`), { signal: s }), draw: (v) => environmentView(v.services, loadEnv) });
  };
  const reload = loadInto(slot, {
    signal,
    load: (s) => get(base(`/stacks/${id}`), { signal: s }),
    draw: (d) => {
      const s = d.stack;
      const running = d.state === 'running' || d.state === 'partial';
      const manage = can('manage');
      const actions = manage || can('operate') ? h('div', { class: 'row' },
        manage ? button({ size: 'sm', variant: 'primary', onClick: () => job('deploy', { pull: false }) }, icon('rocket', 13), d.state === 'down' ? t('pages.stack.deploy') : t('pages.stack.update')) : null,
        running ? button({ size: 'sm', onClick: () => job('stop') }, icon('stop', 13), t('pages.containers.stop')) : d.state === 'stopped' ? button({ size: 'sm', onClick: () => job('start') }, icon('play', 13), t('pages.containers.start')) : null,
        !manage ? null : dropdownMenu({ label: t('common.more'), size: 'sm', items: [
          { label: t('pages.stack.pull_redeploy'), icon: 'download', onSelect: () => job('deploy', { pull: true }) },
          { label: t('pages.stack.pull_images'), icon: 'download', onSelect: () => job('pull') },
          { separator: true },
          { label: t('common.remove'), icon: 'trash', danger: true, onSelect: async () => {
            const ok = await confirm({ title: t('common.remove_name', { name: s.name }), body: t('pages.stack.remove_body'), option: { label: t('pages.stack.remove_volumes'), help: t('pages.stack.remove_volumes_help') } });
            if (ok) job('remove', { volumes: ok.option }, () => navigate('/stacks'));
          } },
        ] })) : null;
      // A Git stack's compose file is its repository's: shown, not edited; its variables are Freightdock's.
      const files = s.git
        ? { file: { value: d.compose, setError: (m) => toast({ text: m, tone: 'critical' }) }, vars: codeEditor({ id: 'stack-env', label: t('pages.stacks.variables'), value: d.env, rows: 6, help: ENV_HELP, language: 'env' }) }
        : stackFiles({ compose: d.compose, env: d.env });
      fileField = files.file;
      if (s.git) files.el = h('div', { class: 'stack' },
        card({ title: s.git.path, sub: s.git.commit ? t('pages.stack.at_commit', { commit: s.git.commit.slice(0, 12) }) : t('pages.stack.not_fetched'), body: h('pre', { class: 'mono pre' }, d.compose || t('pages.stack.deploy_to_fetch')) }), files.vars.el);
      const save = async (deploy) => {
        files.file.setError(''); files.vars.setError('');
        try {
          await put(base(`/stacks/${id}`), { compose: s.git ? '' : files.file.value, env: files.vars.value });
          if (deploy) await job('deploy', { pull: false });
          else { toast({ text: t('pages.stack.saved_deploy'), tone: 'good' }); reload(); }
        } catch (err) { showError(err, files); }
      };
      const editor = h('div', { class: 'stack' }, files.el, can('manage') ? h('div', { class: 'row row--end' },
        button({ onClick: () => save(false) }, t('common.save')), button({ variant: 'primary', onClick: () => save(true) }, icon('rocket', 14), t('pages.stack.save_and_deploy'))) : null);
      const overview = h('div', { class: 'stack' },
        card({ title: t('routes.stack'), body: facts([
          [t('pages.stacks.from'), s.source === 'git' ? 'Git' : t('pages.stack.the_editor')], [t('common.made'), s.created_by ? t('pages.stack.made_by', { when: when(s.created_at), name: s.created_by }) : when(s.created_at)],
          [t('pages.stack.changed'), ago(s.updated_at)], [t('pages.stacks.deployed'), s.deployed_at ? ago(s.deployed_at) : t('common.never')],
          [t('nav.containers'), t('pages.stacks.n_of_running', { running: d.containers.filter((c) => c.State === 'running').length, count: d.containers.length })],
        ]) }),
        card({ title: t('nav.containers'), body: containersTable(d.containers) }),
        accessCard({ path: base(`/stacks/${id}/access`), current: d.access, reload }),
        gitCard(d, reload));
      return h('div', { class: 'stack' },
        pageHeader(h('span', { class: 'row' }, s.name, stackState(d.state)), t('pages.stack.compose_project', { name: s.name }), actions),
        d.docker_error ? callout({ tone: 'critical', title: t('pages.stack.docker_silent'), body: t('pages.stack.docker_error', { reason: d.docker_error }) }) : null,
        tabs({ label: t('routes.stack'), value: tab, onChange: (k) => { tab = k; if (k === 'environment' && !envLoaded) loadEnv(); history.replaceState(null, '', k === 'overview' ? location.pathname : `?tab=${k}`); }, tabs: [
          { key: 'overview', label: t('pages.container.overview'), panel: overview },
          { key: 'environment', label: t('pages.container.environment'), panel: envPanel },
          { key: 'editor', label: s.git ? t('pages.stacks.files') : t('pages.stacks.editor'), panel: editor },
        ] }));
    },
  });
  if (tab === 'environment') loadEnv();
  // The editor is drawn once; a refresh on a timer would throw away what is being typed, so only the overview refreshes.
  every(5000, () => { if (tab === 'overview' && !busy) reload(); });
}

/** A variable whose name says it is a secret: its value is hidden until asked for. */
const SECRET = /pass|secret|token|key|credential|private|auth/i;

/** Every service's variables as the next deploy gives them ("Stacks" 11): one card a service, secrets behind Show. */
function environmentView(services, refresh) {
  const value = (name, v) => {
    if (v === '') return h('span', { class: 'muted' }, t('pages.stack.empty_value'));
    if (!SECRET.test(name)) return h('span', { class: 'mono env-value' }, v);
    const shown = h('span', { class: 'mono env-value' }, '••••••••');
    let open = false;
    const toggle = button({ size: 'sm', variant: 'ghost', onClick: () => { open = !open; shown.textContent = open ? v : '••••••••'; toggle.lastChild.textContent = open ? t('common.hide') : t('common.show'); } }, icon('eye', 13), h('span', null, t('common.show')));
    return h('span', { class: 'row' }, shown, toggle);
  };
  return h('div', { class: 'stack' },
    h('div', { class: 'row env-head' },
      h('p', { class: 'muted' }, t('pages.stack.env_help')),
      button({ size: 'sm', onClick: refresh }, icon('refresh', 13), t('common.refresh'))),
    services.length ? services.map((sv) => card({ title: sv.service, sub: t('pages.container.variables', { count: sv.environment.length }), cls: 'fui-card--flush',
      body: table({ rows: sv.environment, empty: [t('pages.stack.no_variables'), t('pages.stack.sets_none')],
        columns: [{ label: t('common.name'), cls: 'mono', cell: ([k]) => k }, { label: t('pages.stack.value'), cell: ([k, v]) => value(k, v) }] }) }))
      : callout({ title: t('pages.stack.no_services'), body: t('pages.stack.no_services_body') }));
}
