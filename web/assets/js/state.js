// What the app knows about who is signed in and what this browser chose. Per-browser choices live in localStorage under
// `freightdock.*`, read in try/catch: blocked storage is a browser that chose nothing.

const read = (k) => { try { return localStorage.getItem('freightdock.' + k); } catch { return null; } };
const write = (k, v) => { try { if (v == null) localStorage.removeItem('freightdock.' + k); else localStorage.setItem('freightdock.' + k, v); } catch { /* not kept */ } };

export const state = {
  status: null,   // { name, version, configured }
  user: null,     // { id, name, role }
};

export const isAdmin = () => !!state.user && state.user.role === 'admin';
/** Whether the person may operate the stack or node `target` (`kind` 'stack' or 'node'): an administrator, or by a
 *  grant ("Access" 24). */
export const may = (kind, target) => isAdmin() || ((state.user && state.user.grants) || []).some((g) => g.kind === kind && g.target === target);

/** 'device', 'light' or 'dark'. */
export function themeChoice() {
  const theme = read('theme');
  return theme === 'light' || theme === 'dark' ? theme : 'device';
}
export function setTheme(theme) {
  write('theme', theme === 'light' || theme === 'dark' ? theme : null);
  if (theme === 'light' || theme === 'dark') document.documentElement.dataset.theme = theme;
  else delete document.documentElement.dataset.theme;
}

// ---- "new version" hint: a dot on Patch notes until the notes of the version running have been opened here
const SEEN_KEY = 'seenVersion';
const versionSubs = new Set();
export function hasUnseenVersion() {
  const v = state.status && state.status.version;
  return !!v && read(SEEN_KEY) !== v;
}
export function markVersionSeen(v) {
  if (v) write(SEEN_KEY, v);
  versionSubs.forEach((fn) => fn());
}
export function onVersionSeen(fn) { versionSubs.add(fn); return () => versionSubs.delete(fn); }
