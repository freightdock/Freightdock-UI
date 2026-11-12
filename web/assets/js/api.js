// The Freightdock API, from the browser. Every request is same-origin JSON; every change carries the session's CSRF token,
// and an answer of 401 hands the app back to the sign-in page.

import { t } from './i18n.js';

export class ApiError extends Error {
  /** `field`: the form field the server says the error is about, where it says one. */
  constructor(status, message, field = null) {
    super(message);
    this.status = status;
    this.field = field;
  }
}

let csrf = '';
export function setCsrf(token) { csrf = token || ''; }
export const csrfToken = () => csrf;

let onUnauthorized = () => {};
export function setUnauthorizedHandler(fn) { onUnauthorized = fn; }

export function qs(params) {
  const p = new URLSearchParams();
  for (const [k, v] of Object.entries(params || {})) {
    if (v == null || v === '' || v === false) continue;
    p.set(k, v);
  }
  const s = p.toString();
  return s ? '?' + s : '';
}

export const isAbort = (e) => e && e.name === 'AbortError';

/** api('/api/x', { method, body, signal }): the answer's JSON, or null for an empty one. Throws ApiError. */
export async function api(path, { method = 'GET', body, signal, quiet401 = false } = {}) {
  const headers = { Accept: 'application/json' };
  if (body !== undefined) headers['Content-Type'] = 'application/json';
  if (method !== 'GET' && method !== 'HEAD') headers['X-CSRF-Token'] = csrf;
  let r;
  try {
    r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), signal, credentials: 'same-origin' });
  } catch (e) {
    if (isAbort(e)) throw e;
    throw new ApiError(0, t('api.unreachable'));
  }
  const text = await r.text();
  let data = null;
  if (text) { try { data = JSON.parse(text); } catch { data = null; } }
  if (!r.ok) {
    if (r.status === 401 && !quiet401) onUnauthorized();
    throw new ApiError(r.status, (data && data.error) || t('api.answered', { status: String(r.status) }), data && data.field);
  }
  return data;
}

export const get = (path, opts) => api(path, { ...opts, method: 'GET' });
export const post = (path, body, opts) => api(path, { ...opts, method: 'POST', body });
export const put = (path, body, opts) => api(path, { ...opts, method: 'PUT', body });
export const del = (path, opts) => api(path, { ...opts, method: 'DELETE' });

/** A WebSocket to `path` on this server, over wss: when the page is https. */
export function socket(path) {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return new WebSocket(`${proto}//${location.host}${path}`);
}
