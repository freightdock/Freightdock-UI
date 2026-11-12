// Freightdock: terminal. A console in the page: the vendored xterm.js (web/assets/vendor/xterm, MIT) in FinUI's colours,
// read from the tokens when it opens, so a preset and both themes dress it. Data-free in FinUI's sense: it is handed a
// connection (`connect(cols, rows)` answering a WebSocket) and speaks the console protocol of DECISIONS "Streams" 2 over
// it: binary both ways, text only for {"resize":[cols,rows]}. An attach starts with {"attached":{"tty","stdin"}}; when
// stdin is false it only watches, and what is typed is not sent.

import { h } from '../../../finui/core.js';
import { t } from '../../i18n.js';

let loading = null;
function load() {
  if (window.Terminal && window.FitAddon) return Promise.resolve();
  if (loading) return loading;
  const script = (src) => new Promise((ok, no) => { const s = h('script', { src }); s.onload = ok; s.onerror = () => no(new Error(`${src} did not load`)); document.head.append(s); });
  if (!document.querySelector('link[data-xterm]')) document.head.append(h('link', { rel: 'stylesheet', href: '/assets/vendor/xterm/xterm.css', dataset: { xterm: '' } }));
  loading = script('/assets/vendor/xterm/xterm.js').then(() => script('/assets/vendor/xterm/addon-fit.js'));
  return loading;
}

/** The terminal's colours, from FinUI's tokens as they are now. */
function theme(el) {
  const cs = getComputedStyle(el);
  const v = (name, fallback) => cs.getPropertyValue(name).trim() || fallback;
  return { background: v('--bg-sunken', '#111'), foreground: v('--text', '#ddd'), cursor: v('--accent', '#e55'), selectionBackground: v('--selection', '#444') };
}

/**
 * terminal({ connect, onEnd }) → { el, focus(), destroy() }. `connect(cols, rows)` opens the socket; `onEnd(reason)` is
 * told when the console is over (the shell exited, or the connection went).
 */
export function terminal({ connect, onEnd = () => {} }) {
  const el = h('div', { class: 'fd-terminal', role: 'group', 'aria-label': t('components.terminal.label') });
  let term = null, fit = null, ws = null, ro = null, gone = false, input = true;
  const enc = new TextEncoder();
  load().then(() => {
    if (gone) return;
    term = new window.Terminal({ cursorBlink: true, fontFamily: getComputedStyle(el).getPropertyValue('--mono').trim() || 'monospace', fontSize: 13, theme: theme(el), scrollback: 5000, convertEol: false });
    fit = new window.FitAddon.FitAddon();
    term.loadAddon(fit);
    term.open(el);
    fit.fit();
    ws = connect(term.cols, term.rows);
    ws.binaryType = 'arraybuffer';
    ws.onmessage = (e) => {
      if (typeof e.data === 'string') {
        let m = null;
        try { m = JSON.parse(e.data); } catch { /* not ours */ }
        if (m && m.error) { term.write(`\r\n\x1b[31m${m.error}\x1b[0m\r\n`); onEnd(m.error); }
        else if (m && m.end) { term.write(`\r\n\x1b[2m[${t('components.terminal.ended')}]\x1b[0m\r\n`); onEnd('ended'); }
        else if (m && m.attached) { input = m.attached.stdin !== false; if (!input) term.write(`\x1b[2m[${t('components.terminal.watching')}]\x1b[0m\r\n`); }
        return;
      }
      term.write(new Uint8Array(e.data));
    };
    ws.onclose = () => { if (!gone) onEnd('closed'); };
    term.onData((d) => { if (input && ws.readyState === 1) ws.send(enc.encode(d)); });
    const resize = () => { try { fit.fit(); } catch { return; } if (ws.readyState === 1) ws.send(JSON.stringify({ resize: [term.cols, term.rows] })); };
    ro = new ResizeObserver(() => requestAnimationFrame(resize));
    ro.observe(el);
    ws.onopen = () => { resize(); term.focus(); };
  }).catch((e) => { el.append(h('p', { class: 'fui-field__error' }, e.message)); });
  return {
    el,
    focus: () => term && term.focus(),
    destroy() { gone = true; if (ro) ro.disconnect(); if (ws) ws.close(); if (term) term.dispose(); },
  };
}

export const meta = {
  name: 'terminal',
  purpose: 'A console inside a container: what you type goes in, what it prints comes out, full-screen programs included.',
  use: 'Where a person works inside something: a container’s shell, or its main process attached to.',
  avoid: 'Showing output only (logs): that is output-view, which can be searched and kept.',
  a11y: 'A group named Console; xterm.js keeps a screen-reader mode of its own.',
  props: { 'terminal({ connect, onEnd })': 'connect(cols, rows) → WebSocket' },
};
