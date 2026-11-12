// The cluster, as the menus read it: whether this Freightdock is the UI of a Freightdock cluster ("Cluster" 1), asked
// of /api/cluster once a minute at most, and told to whoever listens when that changes.

import { get } from './api.js';

let connected = null;   // null before the first answer
let at = 0;
const subs = new Set();
const FRESH_MS = 60000;

export const clusterConnected = () => connected === true;
export function onClusterChange(fn) { subs.add(fn); return () => subs.delete(fn); }

export async function refreshCluster({ force = false } = {}) {
  if (!force && connected !== null && Date.now() - at < FRESH_MS) return connected;
  let now = connected === true;
  try { now = !!(await get('/api/cluster', { quiet401: true })).connected; } catch { /* keep what was known */ }
  at = Date.now();
  const was = connected;
  connected = now;
  if (was !== now) subs.forEach((fn) => fn());
  return now;
}
