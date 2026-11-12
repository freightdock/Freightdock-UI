// The router: History API addresses, each a page drawn into the shell (or bare, for setup and sign-in). Every page is
// given an AbortSignal and a place to put what must stop when it is left (timers, sockets), torn down on the next
// navigation, so a page never outlives itself.

import { state } from './state.js';

const routes = [];
let current = null;          // { abort, cleanup: [] }
let layouts = null;          // { shell(route) -> content element, bare() -> element }
const listeners = new Set();

/** route('/cluster/stacks', page, { bare, title }). */
export function route(pattern, page, opts = {}) {
  const keys = [];
  const re = new RegExp('^' + pattern.replace(/:[a-z_]+/gi, (m) => { keys.push(m.slice(1)); return '([^/]+)'; }) + '/?$');
  routes.push({ pattern, re, keys, page, ...opts });
}

export function setLayouts(l) { layouts = l; }
export function onRouteChange(fn) { listeners.add(fn); return () => listeners.delete(fn); }

// A parameter goes into an API path: decoded to a separator or a dot segment it would reach another endpoint.
const PATHLIKE = /[/\\?#]|^\.\.?$/;

function match(pathname) {
  for (const r of routes) {
    const m = r.re.exec(pathname);
    if (!m) continue;
    const params = {};
    try { r.keys.forEach((k, i) => { params[k] = decodeURIComponent(m[i + 1]); }); } catch { continue; }
    if (Object.values(params).some((v) => PATHLIKE.test(v))) continue;
    return { route: r, params };
  }
  return null;
}

/** Where a path really goes, given whether Freightdock is set up and somebody is signed in. */
function guard(url) {
  const path = url.pathname;
  if (state.status && !state.status.configured) return path === '/setup' ? null : '/setup';
  if (path === '/setup') return '/';
  if (!state.user) {
    if (path === '/login') return null;
    const next = path + url.search;
    return '/login' + (next && next !== '/' ? '?next=' + encodeURIComponent(next) : '');
  }
  if (path === '/login' || path === '/' || path === '/home') return '/dashboard';
  return null;
}

function teardown() {
  if (!current) return;
  current.abort.abort();
  for (const fn of current.cleanup.splice(0)) { try { fn(); } catch (e) { console.error(e); } }
  current = null;
}

export async function render() {
  const url = new URL(location.href);
  const to = guard(url);
  if (to) { history.replaceState(history.state, '', to); return render(); }
  const m = match(url.pathname) || match('/404');
  teardown();
  const abort = new AbortController();
  current = { abort, cleanup: [] };
  const mine = current;
  const el = m.route.bare ? layouts.bare() : layouts.shell(m.route, url.pathname);
  document.title = m.route.title ? `${typeof m.route.title === 'function' ? m.route.title(m.params) : m.route.title} · Freightdock` : 'Freightdock';
  for (const fn of listeners) fn(url.pathname);
  try {
    await m.route.page({
      el, params: m.params, query: url.searchParams, signal: abort.signal,
      cleanup: (fn) => mine.cleanup.push(fn),
      every: (ms, fn) => { const timer = setInterval(() => { if (!document.hidden) fn(); }, ms); mine.cleanup.push(() => clearInterval(timer)); },
    });
  } catch (e) {
    if (e && e.name === 'AbortError') return;
    console.error(e);
  }
}

export function navigate(to, { replace = false } = {}) {
  if (replace) history.replaceState(null, '', to); else history.pushState(null, '', to);
  render();
  window.scrollTo(0, 0);
}

/** Links inside the app stay inside it. */
export function start() {
  addEventListener('popstate', render);
  document.addEventListener('click', (e) => {
    if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target.closest && e.target.closest('a[href]');
    if (!a || a.target === '_blank' || a.hasAttribute('download')) return;
    const url = new URL(a.href, location.href);
    if (url.origin !== location.origin || url.pathname.startsWith('/api/')) return;
    e.preventDefault();
    if (url.pathname + url.search !== location.pathname + location.search) navigate(url.pathname + url.search + url.hash);
  });
  return render();
}
