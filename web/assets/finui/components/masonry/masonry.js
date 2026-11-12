// FinUI: masonry. Cards of different heights in columns, each placed under the column whose bottom is highest, so a
// short card never leaves a hole beside a tall one. The items stay in their order in the page (the keyboard and a
// screen reader go through them as written); only where they are drawn changes. A browser with native masonry lays them
// out itself; elsewhere this measures each item and places it on a grid of 1px rows. Until it has measured, and
// without script, the items sit in an ordinary grid.

import { h } from '../../core.js';
import { columns, place, height } from './plan.js';

/** The browser lays out masonry itself. */
const native = () => typeof CSS !== 'undefined' && CSS.supports && CSS.supports('grid-template-rows', 'masonry');

/**
 * masonry({ min, label, class }, ...children): the layout; answers { el, layout(), destroy() }. min: the narrowest a
 * column may be, in px (320). A child spans more columns with data-span="2" (cut to the columns there are). label names
 * the region for a screen reader when it is one. layout() places the items again (it does so by itself when the width,
 * an item's height or the items change).
 */
export function masonry({ min = 320, label = null, class: extra = null } = {}, ...children) {
  const el = h('div', { class: ['fui-masonry', extra], role: label ? 'region' : null, 'aria-label': label }, children);
  el.style.setProperty('--fui-masonry-min', `${Number(min) || 320}px`);
  if (native()) {
    el.classList.add('is-native');
    return { el, layout() {}, destroy() {} };
  }
  let queued = false;
  const layout = () => {
    queued = false;
    if (!el.isConnected) return;
    const items = [...el.children].filter((c) => !c.hidden);
    const style = getComputedStyle(el);
    const gap = parseFloat(style.columnGap) || 0;
    const width = el.clientWidth;
    const cols = columns(width, Number(min) || 320, gap);
    if (cols === 1) {
      el.classList.remove('is-laid');
      el.style.removeProperty('height');
      return;
    }
    el.style.setProperty('--fui-masonry-cols', String(cols));
    el.classList.add('is-laid');
    // Each item's height at the width it will have: measured in place, after the column count is set.
    const sized = items.map((c) => ({ height: c.getBoundingClientRect().height, span: c.dataset.span }));
    const placed = place(sized, cols, gap);
    items.forEach((c, i) => {
      const p = placed[i];
      c.style.setProperty('--fui-masonry-col', String(p.col + 1));
      c.style.setProperty('--fui-masonry-span', String(p.span));
      c.style.setProperty('--fui-masonry-row', String(p.row + 1));
      c.style.setProperty('--fui-masonry-rows', String(p.rows));
    });
    el.style.setProperty('height', `${height(placed)}px`);
  };
  const soon = () => { if (!queued) { queued = true; requestAnimationFrame(layout); } };
  const sizes = typeof ResizeObserver === 'function' ? new ResizeObserver(soon) : null;
  const watch = () => { if (sizes) { sizes.disconnect(); sizes.observe(el); [...el.children].forEach((c) => sizes.observe(c)); } soon(); };
  const changes = typeof MutationObserver === 'function' ? new MutationObserver(watch) : null;
  if (changes) changes.observe(el, { childList: true });
  watch();
  return { el, layout: soon, destroy() { if (sizes) sizes.disconnect(); if (changes) changes.disconnect(); } };
}

export const meta = {
  name: 'masonry',
  purpose: 'Lays cards of different heights out in columns with no holes: each goes under the column that ends highest.',
  use: 'A dashboard of cards whose heights differ and whose order is loose: summaries, short lists, a figure. Give it the narrowest a column may be; it makes as many columns as fit, and one on a phone.',
  avoid: 'Tables and long lists: give them the full width. Panels meant to be compared side by side (two charts over the same time): an aligned grid keeps their tops level. Content read strictly in rows.',
  variants: ['one column narrow, as many as fit wide', 'an item two columns wide (data-span="2")', 'native masonry where the browser has it'],
  states: ['an ordinary grid before it has measured and without script', 'placed again when the width, an item or the items change'],
  a11y: 'The items stay in their order in the page, so the keyboard and a screen reader meet them as written; with label it is a named region.',
  props: {
    'masonry({ min, label, class }, ...children)': 'min: 320 (px), the narrowest column; label: names it as a region',
    'data-span on a child': 'how many columns it spans, cut to the columns there are',
    'layout()': 'place the items again (it does so by itself on resize and on change)',
    'columns(width, min, gap), place(items, cols, gap), height(placed) in plan.js': 'the counting and the placing, pure',
  },
  playground: {
    controls: [
      { key: 'min', label: 'Narrowest column', choices: [['180', '180 px'], ['240', '240 px'], ['320', '320 px']] },
      { key: 'wide', label: 'A wide card', choices: [['no', 'None'], ['yes', 'The second spans two']] },
    ],
    render: (o) => {
      const tall = [3, 1, 5, 2, 4, 1, 3, 2];
      const cards = tall.map((n, i) => h('div', { class: 'fui-card fui-masonry__demo', dataset: { span: o.wide === 'yes' && i === 1 ? '2' : '1' } },
        h('strong', null, `Card ${i + 1}`), ...Array.from({ length: n }, () => h('p', null, 'A line of the card.'))));
      return masonry({ min: Number(o.min) }, ...cards).el;
    },
  },
};
