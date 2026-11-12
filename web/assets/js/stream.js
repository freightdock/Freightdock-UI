// Pulls, pushes, builds and loads answer a line of JSON at a time as they go (DECISIONS "Streams" 1). `postStream` reads
// them as they come and hands each to `onLine`; it resolves when the last says done, and throws with Docker's words when
// one says error.

import { ApiError, csrfToken } from './api.js';
import { t } from './i18n.js';

export async function postStream(path, body, onLine, { signal, raw = false, contentType = 'application/json' } = {}) {
  const r = await fetch(path, {
    method: 'POST', signal, credentials: 'same-origin',
    headers: { 'Content-Type': contentType, 'X-CSRF-Token': csrfToken(), Accept: 'application/x-ndjson' },
    body: raw ? body : JSON.stringify(body),
  });
  if (!r.ok) {
    let msg = t('api.answered', { status: String(r.status) }), field = null;
    try { const j = await r.json(); msg = j.error || msg; field = j.field || null; } catch { /* not JSON */ }
    throw new ApiError(r.status, msg, field);
  }
  const reader = r.body.getReader();
  const dec = new TextDecoder();
  let buf = '', done = false;
  for (;;) {
    const { value, done: end } = await reader.read();
    if (value) buf += dec.decode(value, { stream: true });
    let nl;
    while ((nl = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, nl).trim();
      buf = buf.slice(nl + 1);
      if (!line) continue;
      let j;
      try { j = JSON.parse(line); } catch { continue; }
      if (j.error) throw new ApiError(502, j.error);
      if (j.done) done = true; else onLine(j);
    }
    if (end) break;
  }
  if (!done) throw new ApiError(0, t('stream.ended'));
}

/** Docker's pull and push lines, layer by layer, as a running total: { layers: Map(id -> {status, current, total}), percent }. */
export function layerTally() {
  const layers = new Map();
  return {
    layers,
    add(l) {
      if (!l.id || !l.status) return;
      const prev = layers.get(l.id) || {};
      layers.set(l.id, { status: l.status, current: l.current ?? prev.current, total: l.total ?? prev.total });
    },
    get percent() {
      let cur = 0, tot = 0;
      for (const l of layers.values()) {
        if (!l.total) continue;
        tot += l.total;
        cur += /complete|already exists|pushed|mounted/i.test(l.status) ? l.total : Math.min(l.current || 0, l.total);
      }
      return tot ? cur / tot : null;
    },
  };
}
