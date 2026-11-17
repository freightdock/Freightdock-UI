// The Storage page's sums ("Cluster" 43): what the disks or shared storages hold together, and a part's share of a
// whole. A number nobody measured is left out of a sum and counted as unknown, never taken for zero.

/** Total, used and free over `list` (each with total_bytes, used_bytes and maybe free_bytes), the share used, and
 *  how many said no size. */
export function totals(list) {
  let total = 0, used = 0, free = 0, unknown = 0;
  for (const x of list) {
    if (x.total_bytes == null || x.used_bytes == null) { unknown += 1; continue; }
    total += x.total_bytes;
    used += x.used_bytes;
    free += x.free_bytes != null ? x.free_bytes : Math.max(0, x.total_bytes - x.used_bytes);
  }
  return { total, used, free, share: total ? used / total : null, unknown };
}

/** `part` of `whole`, from 0 to 1; null when either is not known or the whole is nothing. */
export function share(part, whole) {
  return part == null || !whole ? null : part / whole;
}
