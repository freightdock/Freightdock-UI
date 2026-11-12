// How a page loads what it shows: a skeleton while it waits, FinUI's error with Try again when it fails, and the page's
// own drawing once it has the data. Plus the table every list is drawn as.

import { h, mount, icon } from './dom.js';
import { isAbort } from './api.js';
import { errorState } from '../finui/components/error/error.js';
import { emptyState } from '../finui/components/empty/empty.js';
import { dataTable } from '../finui/components/data-table/data-table.js';
import { sk } from '../finui/components/skeleton/skeleton.js';
import { t } from './i18n.js';

/** Loads with `load(signal)` into `slot`, drawing with `draw(data)`. Answers `reload()`, which keeps what is shown until the
 *  new answer comes (no flash of skeleton on a refresh). */
export function loadInto(slot, { signal, load, draw, skeleton = () => sk.tableRows(5) }) {
  let first = true, last, has = false;
  async function run() {
    if (first) mount(slot, skeleton());
    try {
      const data = await load(signal);
      if (signal.aborted) return;
      last = data; has = true;
      mount(slot, draw(data, run));
    } catch (e) {
      if (isAbort(e) || signal.aborted) return;
      mount(slot, errorState(e, run));
    } finally {
      first = false;
    }
  }
  run();
  /** Draws what was last loaded again, without asking (a filter changed). */
  run.redraw = () => { if (has) mount(slot, draw(last, run)); };
  return run;
}

/**
 * A FinUI data table. `columns`: [{ label, cell(row) -> node|text, cls, sort(row) }], `rows`: the data. `empty`: [title,
 * text] when there are none. A column's `sort` gives the value the table sorts it by (data-sort), where the text alone
 * would sort wrong.
 */
export function table({ columns, rows, empty = [t('view.nothing_here'), ''], filter = 'auto', dense = false, rowClass = null, label }) {
  if (!rows.length) return emptyState(empty[0], empty[1]);
  const tbl = h('table', { class: ['fui-data-table', 'fui-data-table--hover', dense && 'fui-data-table--dense'] },
    h('thead', null, h('tr', null, columns.map((c) => h('th', { class: c.cls || null, 'data-nosort': c.nosort ? '' : null }, c.label)))),
    h('tbody', null, rows.map((r) => h('tr', { class: rowClass ? rowClass(r) : null }, columns.map((c) => {
      const v = c.cell(r);
      return h('td', { class: c.cls || null, 'data-sort': c.sort ? String(c.sort(r)) : null }, v);
    })))));
  return dataTable(tbl, { filter, label });
}

/** A filter box that lives outside what is redrawn, so a refresh keeps what was typed. `onInput(words)`. */
export function filterBox(label, onInput) {
  const input = h('input', { class: 'fui-field__input fui-field__input--search', type: 'search', placeholder: t('view.filter_placeholder', { label }), 'aria-label': label, autocomplete: 'off' });
  input.addEventListener('input', () => onInput(input.value.toLowerCase().split(/\s+/).filter(Boolean)));
  input.addEventListener('keydown', (e) => { if (e.key === 'Escape' && input.value) { e.stopPropagation(); input.value = ''; onInput([]); } });
  return h('div', { class: 'fui-field__search toolbar__search' }, icon('search', 14), input);
}

/** Whether every word is somewhere in the texts. */
export const matches = (words, ...texts) => { const tbl = texts.filter(Boolean).join(' ').toLowerCase(); return words.every((w) => tbl.includes(w)); };
