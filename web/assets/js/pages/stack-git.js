// Git stacks: the repository form (address, branch, compose file's path, credentials, how often to check), and the card
// on a Git stack's page with where it comes from, the commit it runs, how the last check went, and its settings.

import { h, ago, when } from '../dom.js';
import { put } from '../api.js';
import { base } from '../docker.js';
import { can } from '../state.js';
import { codeEditor, ENV_HELP } from '../editor.js';
import { formField } from '../../finui/components/field/field.js';
import { card } from '../../finui/components/card/card.js';
import { facts } from '../../finui/components/facts/facts.js';
import { callout } from '../../finui/components/callout/callout.js';
import { button } from '../../finui/components/button/button.js';
import { openModal } from '../../finui/components/modal/modal.js';
import { toast } from '../../finui/components/toast/toast.js';
import { t } from '../i18n.js';

/** The repository's fields, filled from `g` (a stack's git settings) when there is one. → { el, value(), setError(err) } */
export function gitFields(g = null, env = '') {
  const url = formField({ id: 'git-url', label: t('pages.stack_git.repository'), placeholder: 'https://github.com/owner/repo.git', autocomplete: 'off' });
  const reference = formField({ id: 'git-ref', label: t('pages.stack_git.branch_or_tag'), placeholder: t('pages.stack_git.default_branch'), autocomplete: 'off' });
  const path = formField({ id: 'git-path', label: t('pages.stacks.compose_file'), placeholder: 'docker-compose.yml', autocomplete: 'off', help: t('pages.stack_git.path_help') });
  const user = formField({ id: 'git-user', label: t('auth.user_name'), autocomplete: 'off', help: t('pages.stack_git.user_help') });
  const token = formField({ id: 'git-token', label: t('pages.stack_git.token'), type: 'password', autocomplete: 'new-password',
    help: g && g.has_token ? t('pages.stack_git.token_kept') : t('pages.stack_git.token_help') });
  const poll = formField({ id: 'git-poll', label: t('pages.stack_git.poll'), type: 'number', inputMode: 'numeric', help: t('pages.stack_git.poll_help') });
  const vars = codeEditor({ id: 'git-env', label: t('pages.stacks.variables'), value: env, rows: 5, help: ENV_HELP, language: 'env' });
  if (g) { url.input.value = g.url; reference.input.value = g.reference || ''; path.input.value = g.path || ''; user.input.value = g.user || ''; poll.input.value = String(Math.round((g.poll_s || 0) / 60)); }
  else poll.input.value = '5';
  const fields = { url, reference, path, user, token, poll_minutes: poll };
  return {
    el: h('div', { class: 'stack' }, url.el, h('div', { class: 'grid-2' }, reference.el, path.el), h('div', { class: 'grid-2' }, user.el, token.el), poll.el, vars.el),
    value: () => ({ url: url.input.value.trim(), reference: reference.input.value.trim(), path: path.input.value.trim(), user: user.input.value.trim() || null,
      token: token.input.value || null, poll_minutes: Number(poll.input.value || 0), env: vars.value }),
    clear() { Object.values(fields).forEach((f) => f.setError('')); vars.setError(''); },
    /** Shows an answer's error by its field; false when it is about none of them. */
    setError(err) {
      if (err.field === 'env') { vars.setError(err.message); return true; }
      const f = fields[err.field];
      if (f) { f.setError(err.message); return true; }
      return false;
    },
  };
}

function editDialog(id, d, reload) {
  const f = gitFields(d.stack.git, d.env);
  const go = button({ variant: 'primary', type: 'submit' }, t('common.save'));
  const form = h('form', { class: 'stack', novalidate: true }, f.el, h('div', { class: 'row row--end' }, go));
  const m = openModal({ title: t('pages.stack_git.where_from'), body: form, wide: true });
  form.addEventListener('submit', async (e) => {
    e.preventDefault(); f.clear(); go.disabled = true;
    try {
      await put(base(`/stacks/${id}/git`), f.value());
      m.close(); toast({ text: t('pages.stack_git.saved'), tone: 'good' }); reload();
    } catch (err) { if (!f.setError(err)) toast({ text: err.message, tone: 'critical' }); } finally { go.disabled = false; }
  });
}

/** The Git card on a stack's page; null for a stack from the editor. */
export function gitCard(d, reload) {
  const g = d.stack.git;
  if (!g) return null;
  const every = g.poll_s ? t('pages.stack_git.every', { count: Math.round(g.poll_s / 60) }) : t('pages.stack_git.when_asked');
  return card({
    title: 'Git', actions: can('manage') ? button({ size: 'sm', onClick: () => editDialog(d.stack.id, d, reload) }, t('security.settings')) : null,
    body: h('div', { class: 'stack' },
      g.error ? callout({ tone: 'critical', title: t('pages.stack_git.last_failed'), body: g.error }) : null,
      facts([
        [t('pages.stack_git.repository'), g.url, { wide: true, mono: true }], [t('pages.stack_git.branch'), g.reference || t('pages.stack_git.the_default')], [t('pages.stacks.compose_file'), g.path, { mono: true }],
        [t('pages.stack_git.commit'), g.commit ? g.commit.slice(0, 12) : t('pages.stack_git.not_deployed'), { mono: true }], [t('pages.stack_git.checks'), every],
        [t('pages.stack_git.last_checked'), g.checked_at ? h('span', { title: when(g.checked_at) }, ago(g.checked_at)) : t('common.never')],
        [t('pages.stack_git.signs_in'), g.user || g.has_token ? (g.has_token ? t('pages.stack_git.signs_in_kept', { who: g.user || t('pages.stack_git.with_token') }) : g.user) : t('common.no')],
      ])),
  });
}
