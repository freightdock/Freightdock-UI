// The words of the interface, in the language this browser reads Freightdock in, the way FinStats does it ("UI" 17). Every
// text a page shows goes through t() (or tParts(), when a placeholder is an element): the catalogues are
// web/assets/i18n/<BCP 47 tag>.json, English the source, and the tests (web.rs) and qa/i18n-check.mjs hold the others to
// its keys, placeholders and plural forms.
//
// The language is this browser's (Account → Language, kept in localStorage like the theme): nothing stored follows the
// browser's own languages. main.js loads the catalogue before any page module runs, so a module may say t('…') at the
// top level; choosing another language reloads the page. FinUI's components speak through the catalogue's `finui`
// section. What the server answers in words (an error's reason) stays English.

import { words as finuiWords } from '../finui/i18n.js';

/** Every language Freightdock speaks, by its own name; the first is the source. theme.js repeats the tags. */
export const LANGUAGES = [
  { tag: 'en', name: 'English' },
  { tag: 'nb', name: 'Norsk bokmål' },
];
const SOURCE = LANGUAGES[0].tag;
const KEY = 'freightdock.language';

/** The catalogue a browser language asks for: its language, Norwegian of any kind being Bokmål. theme.js has the same
 *  rule. */
function languageOf(raw) {
  const lang = String(raw || '').trim().toLowerCase().split('-')[0];
  if (lang === 'no' || lang === 'nn') return 'nb';
  return LANGUAGES.some((l) => l.tag === lang) ? lang : null;
}

/** The first of a browser's languages that Freightdock speaks; nothing usable is English. */
export function pickLanguage(list) {
  for (const raw of list || []) {
    const found = languageOf(raw);
    if (found) return found;
  }
  return SOURCE;
}

/** What this browser chose ('' = follow the browser). */
export function storedLanguage() {
  try { const v = localStorage.getItem(KEY); return (v && languageOf(v)) || ''; } catch { return ''; }
}
/** The language to read Freightdock in: the one chosen here, else the browser's. */
export function chosenLanguage() {
  return storedLanguage() || pickLanguage(typeof navigator !== 'undefined' ? navigator.languages || [navigator.language] : []);
}
/** Keep a choice ('' follows the browser) and read the page again in it. */
export function setLanguage(tag) {
  try { if (tag) localStorage.setItem(KEY, tag); else localStorage.removeItem(KEY); } catch { /* storage blocked: this visit only */ }
  location.reload();
}

let current = SOURCE;
let words = {};
let plurals = new Intl.PluralRules(SOURCE);
let numbers = new Intl.NumberFormat(SOURCE);

/** The language in use, for every Intl call and localeCompare. */
export const lang = () => current;

/** Speak `tag` from `catalogue` (its JSON). */
export function useCatalogue(tag, catalogue) {
  current = tag;
  words = catalogue || {};
  plurals = new Intl.PluralRules(tag);
  numbers = new Intl.NumberFormat(tag);
  // The table reader reads durations back as this language writes them.
  finuiWords({ ...words.finui, units: words.units || {} }, tag);
}

/** Fetch the chosen language's catalogue and put it in use; English if it cannot be had. */
export async function loadLanguage(tag = chosenLanguage()) {
  try {
    const res = await fetch(`/assets/i18n/${tag}.json`);
    if (!res.ok) throw new Error(String(res.status));
    useCatalogue(tag, await res.json());
  } catch {
    if (tag !== SOURCE) return loadLanguage(SOURCE);
  }
  if (typeof document !== 'undefined') document.documentElement.lang = current;
  return current;
}

const FORMS = ['zero', 'one', 'two', 'few', 'many', 'other'];
/** A text that depends on a number: an object of CLDR plural forms, `other` among them. */
const isPlural = (v) => !!v && typeof v === 'object' && typeof v.other === 'string' && Object.keys(v).every((k) => FORMS.includes(k));
const missing = new Set();
function find(key) {
  let v = words;
  for (const part of String(key).split('.')) v = v && typeof v === 'object' ? v[part] : undefined;
  return typeof v === 'string' || isPlural(v) ? v : undefined;
}
/** The text of `key` (a plural picks its form by `vars.count`), its placeholders not yet filled; null if there is none. */
function text(key, vars) {
  const v = find(key);
  if (typeof v === 'string') return v;
  if (v) {
    const n = vars && vars.count != null ? Number(vars.count) : NaN;
    return v[Number.isFinite(n) ? plurals.select(n) : 'other'] ?? v.other;
  }
  if (!missing.has(key)) { missing.add(key); console.warn(`i18n: no text for ${key} in ${current}`); }
  return null;
}
const filled = (value) => (typeof value === 'number' ? numbers.format(value) : String(value ?? ''));

/** `key` in the language in use, `{name}` filled from `vars` (numbers written the language's way). A key with no text
 *  is the key itself, so a gap shows as one rather than as nothing. */
export function t(key, vars) {
  const s = text(key, vars);
  if (s == null) return String(key);
  return vars ? s.replace(/\{([a-z_]+)\}/g, (m, name) => (name in vars ? filled(vars[name]) : m)) : s;
}

/** As t(), but `fallback` (said as it is) where the catalogue has no text for `key`: for ids from the server that the
 *  catalogue may not know (a new kind of event, an action of the activity log). */
export function tOr(key, fallback, vars) {
  return find(key) !== undefined ? t(key, vars) : fallback;
}

/** As t(), for a text with an element in it: an array for h() of the text around each placeholder and what fills it,
 *  a node left a node ("Signed in as <strong>ola</strong>."). */
export function tParts(key, vars = {}) {
  const s = text(key, vars);
  if (s == null) return [String(key)];
  const out = [];
  let at = 0;
  const push = (x) => { if (typeof x === 'string' && typeof out[out.length - 1] === 'string') out[out.length - 1] += x; else if (x !== '') out.push(x); };
  for (const m of s.matchAll(/\{([a-z_]+)\}/g)) {
    if (!(m[1] in vars)) continue;
    const value = vars[m[1]];
    push(s.slice(at, m.index));
    at = m.index + m[0].length;
    push(value && typeof value === 'object' ? value : filled(value));
  }
  push(s.slice(at));
  return out;
}
