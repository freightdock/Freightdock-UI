// FinUI: masonry's rules, apart from any page. How many columns a width holds, and where each item goes: into the
// column (or the run of columns, for a wide item) whose bottom is highest, in the items' own order, so the page reads
// much as it is written. Pure, so a test reads it without a page.

/** How many columns of at least `min` px fit in `width` px with `gap` px between them; one at the least. */
export function columns(width, min, gap) {
  if (!(width > 0) || !(min > 0)) return 1;
  return Math.max(1, Math.floor((width + gap) / (min + gap)));
}

/**
 * Where each item goes. items: [{ height, span }] in px and columns (a span wider than `cols` is cut to it). Answers
 * [{ col, row, span, rows }]: the first column (from 0), the row it starts on (in px, as the grid's 1px rows count
 * them, from 0), how many columns it spans and how many rows it takes. `gap` px are left below each item.
 */
export function place(items, cols, gap) {
  const n = Math.max(1, cols | 0);
  const bottom = new Array(n).fill(0);
  return items.map((it) => {
    const span = Math.min(n, Math.max(1, Math.round(Number(it.span) || 1)));
    const rows = Math.max(1, Math.ceil(Number(it.height) || 0));
    let best = 0, top = Infinity;
    for (let c = 0; c + span <= n; c++) {
      const at = Math.max(...bottom.slice(c, c + span));
      if (at < top) { top = at; best = c; }
    }
    for (let c = best; c < best + span; c++) bottom[c] = top + rows + gap;
    return { col: best, row: top, span, rows };
  });
}

/** How tall the whole layout is, in px: down to the lowest item's bottom, without the gap below it. */
export function height(placed) {
  return placed.reduce((most, p) => Math.max(most, p.row + p.rows), 0);
}
