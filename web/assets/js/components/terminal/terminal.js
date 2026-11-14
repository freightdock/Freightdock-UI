// Freightdock: terminal. A shell on a node or in a container ("Cluster" 35) in xterm.js (web/assets/vendor/xterm,
// MIT, pre-built), over a WebSocket to Freightdock, which hands it to the managers and they to the node's worker. It
// first asks whether the shell may be opened (a browser is never told why a WebSocket failed), then opens it: what
// the shell writes comes as binary messages and its end as a text one ({"end": {exit_code, error}}); what is typed
// goes as binary and a new size as text ({"resize": {cols, rows}}). Its colours are FinUI's tokens, read as the page
// draws them; the sixteen colours programs ask for are the terminal's own, as in any terminal.

import { get } from '../../api.js';

let loading = null;
/** xterm.js and its stylesheet, once for the whole page. */
const load = () => (loading ||= (() => {
  if (!document.querySelector('link[data-fd-xterm]')) {
    const link = Object.assign(document.createElement('link'), { rel: 'stylesheet', href: '/assets/vendor/xterm/xterm.css' });
    link.dataset.fdXterm = '';
    document.head.append(link);
  }
  return import('/assets/vendor/xterm/xterm.js');
})());

/** The path of a shell, the host's when `container` is empty. */
export function shellPath(node, container = '', cols = 0, rows = 0) {
  const q = new URLSearchParams({ container: container || '' });
  if (cols) q.set('cols', String(cols));
  if (rows) q.set('rows', String(rows));
  return `/api/cluster/nodes/${encodeURIComponent(node)}/shell?${q}`;
}

/** The WebSocket's address for a path of this page's origin. */
export function socketUrl(path, loc = location) {
  return `${loc.protocol === 'https:' ? 'wss' : 'ws'}://${loc.host}${path}`;
}

/** What a text message from the shell says: its end, or nothing known. */
export function readText(text) {
  try {
    const v = JSON.parse(text);
    return v && v.end ? { end: { exitCode: Number(v.end.exit_code), error: String(v.end.error || '') } } : null;
  } catch { return null; }
}

/** A token's colour as the page draws it now (light or dark, the preset chosen). */
function tokenColour(probe, token) {
  probe.style.color = `var(${token})`;
  return getComputedStyle(probe).color;
}

function theme(el) {
  const probe = document.createElement('span');
  probe.hidden = true;
  el.append(probe);
  const c = (tk) => tokenColour(probe, tk);
  const th = { background: c('--bg-sunken'), foreground: c('--text'), cursor: c('--accent'), cursorAccent: c('--bg-sunken'), selectionBackground: c('--accent-wash') };
  const font = getComputedStyle(el).getPropertyValue('--mono').trim() || 'monospace';
  probe.remove();
  return { th, font };
}

/**
 * openTerminal(el, { node, container, onState }): the shell drawn into `el`. `onState(state, detail)` is told
 * 'connecting', 'open', 'ended' ({exitCode, error}) or 'failed' (the reason). Gives back { close(), focus() }.
 */
export function openTerminal(el, { node, container = '', onState = () => {} }) {
  let ws = null, term = null, observer = null, closed = false;
  const close = () => {
    closed = true;
    observer?.disconnect();
    try { ws?.close(); } catch { /* already closed */ }
    term?.dispose();
  };
  (async () => {
    onState('connecting');
    try { await get(shellPath(node, container)); } catch (e) { if (!closed) onState('failed', e.message); return; }
    let mod;
    try { mod = await load(); } catch (e) { if (!closed) onState('failed', e.message); return; }
    if (closed) return;
    const { th, font } = theme(el);
    term = new mod.Terminal({ cursorBlink: true, fontFamily: font, fontSize: 13, scrollback: 5000, theme: th, allowProposedApi: false });
    const fit = new mod.FitAddon();
    term.loadAddon(fit);
    term.open(el);
    try { fit.fit(); } catch { /* not laid out yet */ }
    ws = new WebSocket(socketUrl(shellPath(node, container, term.cols, term.rows)));
    ws.binaryType = 'arraybuffer';
    const enc = new TextEncoder();
    let ended = false;
    ws.onopen = () => { onState('open'); term.focus(); };
    ws.onmessage = (m) => {
      if (typeof m.data === 'string') {
        const said = readText(m.data);
        if (said) { ended = true; onState('ended', said.end); }
        return;
      }
      term.write(new Uint8Array(m.data));
    };
    ws.onclose = () => { if (!ended && !closed) onState('ended', { exitCode: -1, error: '' }); };
    term.onData((d) => { if (ws.readyState === WebSocket.OPEN) ws.send(enc.encode(d)); });
    term.onBinary((d) => { if (ws.readyState === WebSocket.OPEN) ws.send(Uint8Array.from(d, (ch) => ch.charCodeAt(0) & 0xff)); });
    term.onResize(({ cols, rows }) => { if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify({ resize: { cols, rows } })); });
    observer = new ResizeObserver(() => { try { fit.fit(); } catch { /* hidden */ } });
    observer.observe(el);
  })();
  return { close, focus: () => term?.focus() };
}
