// Freightdock: output-view. Lines as they come: kept to the last few thousand, standard error marked, colours and cursor
// codes taken out (they are a terminal's, and the text is what is read), a filter that keeps the matching lines, and
// following the end until the person scrolls up. Data-free: it is handed lines with `append(stream, text)`.

import { h, icon } from '../../../finui/core.js';
import { t } from '../../i18n.js';

// Escape sequences: colours, cursor movement, titles.
const ANSI = /\x1b\[[0-9;?]*[ -\/]*[@-~]|\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b[@-Z\\-_]/g;
export const plain = (s) => s.replace(ANSI, '').replace(/\r(?!\n)/g, '');

/** outputView({ max, label }) → { el, append(stream, text), clear(), setFilter(text), follow(on) }. */
export function outputView({ max = 5000, label = t('components.output_view.label') } = {}) {
  const box = h('div', { class: 'fd-output', role: 'log', 'aria-label': label, 'aria-live': 'off', tabindex: '0' });
  const lines = [];        // { stream, text, el }
  let pending = [], partial = { out: '', err: '' }, filter = '', following = true, frame = 0;
  const shows = (text) => !filter || text.toLowerCase().includes(filter);
  function flush() {
    frame = 0;
    const frag = document.createDocumentFragment();
    for (const l of pending) {
      l.el = h('div', { class: ['fd-output__line', l.stream === 'err' && 'fd-output__line--err'], hidden: !shows(l.text) }, l.text || ' ');
      frag.append(l.el);
      lines.push(l);
    }
    pending = [];
    box.append(frag);
    while (lines.length > max) lines.shift().el.remove();
    if (following) box.scrollTop = box.scrollHeight;
  }
  box.addEventListener('scroll', () => { following = box.scrollHeight - box.scrollTop - box.clientHeight < 24; });
  return {
    el: box,
    /** Text as it came, lines split here; a line not finished waits for the rest. */
    append(stream, text) {
      const s = stream === 'err' ? 'err' : 'out';
      const parts = (partial[s] + plain(text)).split('\n');
      partial[s] = parts.pop();
      for (const text of parts) pending.push({ stream: s, text: text });
      if (!frame) frame = requestAnimationFrame(flush);
    },
    /** Whatever is waiting for its end of line, said now (the stream ended). */
    end() { for (const s of ['out', 'err']) if (partial[s]) { pending.push({ stream: s, text: partial[s] }); partial[s] = ''; } if (!frame) frame = requestAnimationFrame(flush); },
    clear() { lines.splice(0).forEach((l) => l.el.remove()); pending = []; partial = { out: '', err: '' }; },
    setFilter(text) { filter = String(text || '').toLowerCase(); for (const l of lines) l.el.hidden = !shows(l.text); },
    follow(on = true) { following = on; if (on) box.scrollTop = box.scrollHeight; },
    note(text) { box.append(h('div', { class: 'fd-output__note' }, icon('info', 13), text)); if (following) box.scrollTop = box.scrollHeight; },
  };
}

export const meta = {
  name: 'output-view',
  purpose: 'A program’s output as it is written: followed at the end, filtered, the errors marked.',
  use: 'A container’s logs, a build’s output.',
  avoid: 'Typing into it: a console is terminal.',
  a11y: 'role=log, focusable to scroll by keyboard; not announced line by line (aria-live off), which would drown a screen reader.',
  props: { 'outputView({ max, label })': 'append(stream, text), end(), clear(), setFilter(text), follow(on), note(text)' },
};
