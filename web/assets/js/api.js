// The Freightdock API, from the browser. Every request is same-origin JSON; every change carries the session's CSRF token,
// and an answer of 401 hands the app back to the sign-in page.

import { t } from './i18n.js';

export class ApiError extends Error {
  /** `field`: the form field the server says the error is about, where it says one; `code`: what the page can do about it. */
  constructor(status, message, field = null, code = null) {
    super(message);
    this.status = status;
    this.field = field;
    this.code = code;
  }
}

let csrf = '';
export function setCsrf(token) { csrf = token || ''; }
export const csrfToken = () => csrf;

let onUnauthorized = () => {};
export function setUnauthorizedHandler(fn) { onUnauthorized = fn; }

// Asked when the server wants the password again before something that needs a recent sign-in ("Sign-in" 18): answers
// true once it is given, and the request is sent once more.
let onConfirm = async () => false;
export function setConfirmHandler(fn) { onConfirm = fn; }

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

/** api('/api/x', { method, body, signal }): the answer's JSON, or null for an empty one. Throws ApiError. A body that
 *  is a file (a Blob) or bytes goes as it is; anything else as JSON. */
export async function api(path, { method = 'GET', body, signal, quiet401 = false, asked = false } = {}) {
  const headers = { Accept: 'application/json' };
  const raw = body instanceof Blob || body instanceof ArrayBuffer;
  if (body !== undefined) headers['Content-Type'] = raw ? 'application/octet-stream' : 'application/json';
  if (method !== 'GET' && method !== 'HEAD') headers['X-CSRF-Token'] = csrf;
  let r;
  try {
    r = await fetch(path, { method, headers, body: body === undefined ? undefined : raw ? body : JSON.stringify(body), signal, credentials: 'same-origin' });
  } catch (e) {
    if (isAbort(e)) throw e;
    throw new ApiError(0, t('api.unreachable'));
  }
  const text = await r.text();
  let data = null;
  if (text) { try { data = JSON.parse(text); } catch { data = null; } }
  if (!r.ok) {
    if (r.status === 401 && !quiet401) onUnauthorized();
    const code = data && data.code;
    if (r.status === 403 && code === 'confirm' && !asked && await onConfirm()) return api(path, { method, body, signal, quiet401, asked: true });
    throw new ApiError(r.status, (data && data.error) || t('api.answered', { status: String(r.status) }), data && data.field, code);
  }
  return data;
}

export const get = (path, opts) => api(path, { ...opts, method: 'GET' });
export const post = (path, body, opts) => api(path, { ...opts, method: 'POST', body });
export const put = (path, body, opts) => api(path, { ...opts, method: 'PUT', body });
export const del = (path, opts) => api(path, { ...opts, method: 'DELETE' });

/** POSTs `body` to `path` and saves the answer as a file (a backup): the name the server gives, else `fallback`.
 *  Throws ApiError as `api` does. */
export async function download(path, body, fallback) {
  let r;
  try {
    r = await fetch(path, { method: 'POST', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': csrf }, body: JSON.stringify(body), credentials: 'same-origin' });
  } catch {
    throw new ApiError(0, t('api.unreachable'));
  }
  if (!r.ok) {
    let data = null;
    try { data = await r.json(); } catch { data = null; }
    if (r.status === 401) onUnauthorized();
    throw new ApiError(r.status, (data && data.error) || t('api.answered', { status: String(r.status) }));
  }
  const named = /filename="([^"]+)"/.exec(r.headers.get('Content-Disposition') || '');
  const url = URL.createObjectURL(await r.blob());
  const a = document.createElement('a');
  a.href = url;
  a.download = named ? named[1] : fallback;
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}

/** A WebSocket to `path` on this server, over wss: when the page is https. */
export function socket(path) {
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  return new WebSocket(`${proto}//${location.host}${path}`);
}
