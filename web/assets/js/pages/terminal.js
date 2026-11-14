// A shell ("Cluster" 35), for an administrator: on the node itself (a login shell as root) or, with ?container=, in
// one of its containers, in the terminal component across the page. Whether it is connecting, open or ended is said
// above it, with the exit code or why; "Open again" starts a new one. Leaving the page ends it.

import { h, mount } from '../dom.js';
import { get } from '../api.js';
import { pageHeader } from '../../finui/components/page-header/page-header.js';
import { status } from '../../finui/components/badge/badge.js';
import { button } from '../../finui/components/button/button.js';
import { callout } from '../../finui/components/callout/callout.js';
import { t } from '../i18n.js';
import { nodeName } from './cluster.js';
import { openTerminal } from '../components/terminal/terminal.js';

const TONES = { connecting: 'info', open: 'good', ended: 'warning', failed: 'critical' };

/** What the state line says for a state and its detail. */
export function stateWords(state, detail) {
  if (state === 'ended') return detail && detail.error ? t('pages.terminal.ended_why', { why: detail.error }) : detail && detail.exitCode >= 0 ? t('pages.terminal.ended_code', { code: detail.exitCode }) : t('pages.terminal.ended');
  return t(`pages.terminal.state.${state}`);
}

export function terminalPage({ el, params, query, signal }) {
  const id = params.id;
  const container = (query && query.get('container')) || '';
  const head = h('div'), line = h('div', { class: 'row', 'aria-live': 'polite' }), why = h('div'), box = h('div', { class: 'fd-terminal' });
  el.append(head, line, why, box);
  let session = null;
  const back = container ? `/cluster/nodes/${encodeURIComponent(id)}/docker` : `/cluster/nodes/${encodeURIComponent(id)}`;
  const header = (name) => mount(head, pageHeader(container ? t('pages.terminal.title_container', { container, node: name }) : t('pages.terminal.title_host', { node: name }),
    container ? t('pages.terminal.sub_container') : t('pages.terminal.sub_host'),
    h('div', { class: 'row' }, button({ size: 'sm', href: back }, t('pages.terminal.back')), button({ size: 'sm', onClick: start }, t('pages.terminal.again')))));
  function start() {
    session?.close();
    mount(box);
    mount(why);
    session = openTerminal(box, {
      node: id, container,
      onState: (state, detail) => {
        mount(line, status({ tone: TONES[state] || 'info', line: true }, stateWords(state, detail)));
        if (state === 'failed') mount(why, callout({ tone: 'critical', title: t('pages.terminal.failed'), body: detail }));
      },
    });
  }
  header(id);
  get(`/api/cluster/nodes/${encodeURIComponent(id)}`, { signal }).then((n) => header(nodeName(n))).catch(() => {});
  start();
  signal.addEventListener('abort', () => session?.close());
}
