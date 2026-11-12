// The app's own small helpers over FinUI's core: the brand mark, and numbers, sizes and times as people read them, in the
// language in use (i18n.js).

import { h, s } from '../finui/core.js';
import { t, lang } from './i18n.js';
export { h, s, icon, mount, clear } from '../finui/core.js';

/** Freightdock's mark: a container riding the waves, drawn in the text's colour, its parts in tokens (app.css). */
export function brandMark(size = 24) {
  return s('svg', { class: 'brand-mark', viewBox: '0 0 64 64', width: size, height: size, 'aria-hidden': 'true' },
    s('g', { class: 'brand-mark__box', fill: 'none', 'stroke-width': '4', 'stroke-linecap': 'round', 'stroke-linejoin': 'round' },
      s('rect', { x: '13', y: '14', width: '38', height: '23', rx: '3' }), s('path', { d: 'M22.5 14v23M32 14v23M41.5 14v23' })),
    s('path', { class: 'brand-mark__sea', d: 'M6 47c5 0 5-4 10-4s5 4 10 4 5-4 10-4 5 4 10 4 5-4 10-4', fill: 'none', 'stroke-width': '4', 'stroke-linecap': 'round' }));
}

export function brand(size = 24) {
  return h('span', { class: 'brand' }, brandMark(size), h('span', { class: 'brand-name' }, 'Freightdock'));
}

const UNITS = ['B', 'KB', 'MB', 'GB', 'TB', 'PB'];
const formats = new Map();
/** An Intl.NumberFormat for the language in use, made once per set of options. */
function nf(opts = {}) {
  const k = lang() + JSON.stringify(opts);
  if (!formats.has(k)) formats.set(k, new Intl.NumberFormat(lang(), opts));
  return formats.get(k);
}
/** 1234.5 -> "1,235" (en), "1 235" (nb); `digits` after the separator, exactly. */
export const num = (n, digits = 0) => nf({ minimumFractionDigits: digits, maximumFractionDigits: digits }).format(Number(n) || 0);
/** 12.34 -> "12.3%" (en), "12,3 %" (nb): a share already in hundreds. */
export const pct = (n, digits = 1) => t('units.percent', { n: num(n, digits) });

/** 1536 -> "1.5 KB" (binary steps, the way data-table reads them back, in the language's separators). */
export function bytes(n) {
  let v = Number(n) || 0, i = 0;
  while (Math.abs(v) >= 1024 && i < UNITS.length - 1) { v /= 1024; i++; }
  return `${num(v, i === 0 || v >= 100 ? 0 : 1)} ${UNITS[i]}`;
}

const rtf = () => new Intl.RelativeTimeFormat(lang(), { numeric: 'auto' });
/** A Unix time in seconds as "3 minutes ago". */
export function ago(sec) {
  if (!sec) return '';
  const d = Math.round(sec - Date.now() / 1000);
  const a = Math.abs(d);
  if (a < 60) return rtf().format(Math.round(d), 'second');
  if (a < 3600) return rtf().format(Math.round(d / 60), 'minute');
  if (a < 86400) return rtf().format(Math.round(d / 3600), 'hour');
  if (a < 86400 * 30) return rtf().format(Math.round(d / 86400), 'day');
  if (a < 86400 * 365) return rtf().format(Math.round(d / (86400 * 30)), 'month');
  return rtf().format(Math.round(d / (86400 * 365)), 'year');
}

/** A length of time in seconds as "3 hours", in its largest whole unit. */
export function span(sec) {
  const a = Math.max(0, Math.round(Number(sec) || 0));
  const [unit, n] = a < 60 ? ['second', a] : a < 3600 ? ['minute', a / 60] : a < 86400 ? ['hour', a / 3600] : a < 86400 * 30 ? ['day', a / 86400]
    : a < 86400 * 365 ? ['month', a / (86400 * 30)] : ['year', a / (86400 * 365)];
  return nf({ style: 'unit', unit, unitDisplay: 'long', maximumFractionDigits: 0 }).format(Math.round(n));
}

/** A Unix time in seconds as a local date and time. */
export function when(sec) {
  return sec ? new Date(sec * 1000).toLocaleString(lang(), { dateStyle: 'medium', timeStyle: 'short' }) : '';
}

/** A Date as the time of day, to the second when `seconds`. */
export const clock = (d, seconds = false) => d.toLocaleTimeString(lang(), { hour: '2-digit', minute: '2-digit', ...(seconds ? { second: '2-digit' } : {}) });
/** A Date as a short day ("10 Nov"). */
export const day = (d) => d.toLocaleDateString(lang(), { day: 'numeric', month: 'short' });

/** A container's or an image's id as people read it: the first 12 hex digits. */
export const shortId = (id) => String(id || '').replace(/^sha256:/, '').slice(0, 12);
