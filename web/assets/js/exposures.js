// Exposures with "both" ("Cluster" 42): the managers keep one rule per protocol, tcp or udp; the editor and the
// dashboard show a port exposed on both alike as one row, `both`, and the editor writes it back as the two rules.

const same = (a, b) => a.service === b.service && a.container_port === b.container_port && a.address === b.address && a.port === b.port;

/** The exposures with each tcp rule and the udp rule alike in all else made one, protocol `both`, where the first was. */
export function mergeBoth(list) {
  const out = [];
  const used = new Set();
  list.forEach((x, i) => {
    if (used.has(i)) return;
    const j = list.findIndex((y, k) => k > i && !used.has(k) && same(x, y) && ((x.protocol === 'tcp' && y.protocol === 'udp') || (x.protocol === 'udp' && y.protocol === 'tcp')));
    if (j < 0) { out.push(x); return; }
    used.add(j);
    out.push({ ...x, protocol: 'both' });
  });
  return out;
}

/** The exposures with each `both` written as its tcp and its udp rule. */
export function splitBoth(list) {
  return list.flatMap((x) => (x.protocol === 'both' ? [{ ...x, protocol: 'tcp' }, { ...x, protocol: 'udp' }] : [x]));
}

/** Whether a row's protocol takes `p` (tcp or udp). */
export const covers = (protocol, p) => protocol === p || protocol === 'both';
