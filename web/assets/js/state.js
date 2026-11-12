// What the app knows about who is signed in and what this browser chose. Per-browser choices live in localStorage under
// `freightdock.*`, read in try/catch: blocked storage is a browser that chose nothing.

const read = (k) => { try { return localStorage.getItem('freightdock.' + k); } catch { return null; } };
const write = (k, v) => { try { if (v == null) localStorage.removeItem('freightdock.' + k); else localStorage.setItem('freightdock.' + k, v); } catch { /* not kept */ } };

export const state = {
  status: null,   // { name, version, configured }
  user: null,     // { id, name, role }
  env: Number(read('env')) || null,   // the environment the host pages are about, kept per browser; none after sign-in
  envs: [],       // every environment, as /api/environments last said
};

// ---- the environment the Docker pages show
const envSubs = new Set();
/** Chooses the host the pages show; null for none (Home asks which). */
export function setEnv(id) {
  state.env = Number(id) || null;
  write('env', state.env ? String(state.env) : null);
  envSubs.forEach((fn) => fn());
}
export function onEnvChange(fn) { envSubs.add(fn); return () => envSubs.delete(fn); }
/** The environments as the server says; the one shown, when it is gone, is none. */
export function setEnvs(list) {
  state.envs = list || [];
  if (state.env && state.envs.length && !state.envs.some((e) => e.id === state.env)) { setEnv(null); return; }
  envSubs.forEach((fn) => fn());
}
export const currentEnv = () => state.envs.find((e) => e.id === state.env) || null;

export const isAdmin = () => !!state.user && state.user.role === 'admin';

/** Whether the person's role on a host (the one shown, by default) allows `action`: read, operate, console, create,
 *  manage or administer ("Access" 2). The server says which (`actions` in /api/environments) and is the guard all the
 *  same; this only hides what would be refused. */
export const can = (action, env = currentEnv()) => isAdmin() || !!(env && (env.actions || []).includes(action));

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

// ---- how often a container's live stats come ("Streams" 5): kept per browser, 5 seconds unless chosen
export const STATS_EVERY = [1, 5, 10, 30];
export function statsEvery() { const n = Number(read('statsEvery')); return STATS_EVERY.includes(n) ? n : 5; }
export function setStatsEvery(n) { write('statsEvery', STATS_EVERY.includes(n) ? String(n) : null); }
