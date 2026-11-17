// Which workers can keep a stack's data ("Cluster" 40): the deployer hides the rest from the workers to choose and
// says why. A worker says which storage drivers it can mount (none in an unprivileged container); one too old to say
// is judged by how the storage does there (the Shares card's health), and one with neither is kept, as the managers
// keep it. A path storage is a directory the host already has, never mounted, so it rules out nobody.

/** Node id to why it cannot keep data on `used` (storage names), of `storage` (the cluster's, with their health): the
 *  worker's own words (`text`), else the storage and the driver it has no client for. */
export function unfit(nodes, storage, used) {
  const out = new Map();
  const mine = storage.filter((s) => used.includes(s.name) && s.driver !== 'path');
  for (const n of nodes) {
    const r = n.report || {};
    for (const s of mine) {
      if (Array.isArray(r.mounts)) {
        if (!r.mounts.includes(s.driver)) out.set(n.id, !r.mounts.length && r.mounts_why ? { storage: s.name, text: r.mounts_why } : { storage: s.name, driver: s.driver });
      } else {
        const h = (s.health || []).find((x) => x.node_id === n.id);
        if (h && !h.ok) out.set(n.id, h.error ? { storage: s.name, text: h.error } : { storage: s.name, driver: s.driver });
      }
      if (out.has(n.id)) break;
    }
  }
  return out;
}
