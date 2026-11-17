// The words of the UI, held to their catalogues ("UI" 17): node qa/i18n-check.mjs <web dir>. Fails when
//  - a key a module asks for (t('…'), tOr, tParts) has no English text, or a key built at run time (t(`a.${b}`)) has
//    no English text under its fixed start;
//  - English has a text nothing asks for (FinUI's own `finui` and `units` are FinUI's to ask for);
//  - another language lacks a key English has, has one English lacks, fills other {placeholders}, or has other plural
//    forms than its language's (Intl.PluralRules says which);
//  - any text has an em-dash;
//  - a module imports a name the module it names does not export;
//  - a module names something else t, tOr or tParts (a variable or a parameter): a t() beside it would call that;
//  - a string in a module looks like words a person reads and is not said through t() (a heuristic: a sentence, or a
//    capitalised word, outside the places that hold classes, ids, addresses and the like; ON_PURPOSE lists the rest).
// Run by qa/js-check.sh in its node container; node alone, nothing to install.

import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, dirname } from 'node:path';

const web = process.argv[2] || 'web';
const js = join(web, 'assets/js');
const i18n = join(web, 'assets/i18n');
const problems = [];
const fail = (msg) => problems.push(msg);

// ---- the catalogues
const LANGS = readdirSync(i18n).filter((f) => f.endsWith('.json')).map((f) => f.slice(0, -5));
const SOURCE = 'en';
const FORMS = ['zero', 'one', 'two', 'few', 'many', 'other'];
const isPlural = (v) => !!v && typeof v === 'object' && typeof v.other === 'string' && Object.keys(v).every((k) => FORMS.includes(k));
function flatten(o, at = '', out = new Map()) {
  for (const [k, v] of Object.entries(o)) {
    if (!at && k === '$meta') continue;
    const key = at ? `${at}.${k}` : k;
    if (typeof v === 'string' || isPlural(v)) out.set(key, v);
    else if (v && typeof v === 'object') flatten(v, key, out);
    else fail(`${key}: neither a text nor a group`);
  }
  return out;
}
const placeholders = (v) => [...new Set((typeof v === 'string' ? [v] : Object.values(v)).flatMap((s) => [...String(s).matchAll(/\{([a-z_]+)\}/g)].map((m) => m[1])))].sort().join(',');
const cats = {};
for (const l of LANGS) {
  let raw;
  try { raw = JSON.parse(readFileSync(join(i18n, `${l}.json`), 'utf8')); } catch (e) { fail(`${l}.json does not parse: ${e.message}`); continue; }
  if (!raw.$meta || raw.$meta.tag !== l) fail(`${l}.json: $meta.tag is not ${l}`);
  cats[l] = flatten(raw);
}
const en = cats[SOURCE] || new Map();
for (const l of LANGS) {
  const c = cats[l];
  if (!c) continue;
  const forms = new Intl.PluralRules(l).resolvedOptions().pluralCategories.slice().sort().join(',');
  for (const [k, v] of c) {
    if (JSON.stringify(v).includes('\u2014')) fail(`${l}: ${k} has an em-dash`);
    if (isPlural(v) && Object.keys(v).sort().join(',') !== forms) fail(`${l}: ${k} has the forms ${Object.keys(v).sort()}, ${l} has ${forms}`);
    if (l === SOURCE) continue;
    if (!en.has(k)) { fail(`${l}: ${k} is not in ${SOURCE}.json`); continue; }
    if (placeholders(v) !== placeholders(en.get(k))) fail(`${l}: ${k} fills {${placeholders(v)}}, ${SOURCE} {${placeholders(en.get(k))}}`);
    if (isPlural(v) !== isPlural(en.get(k))) fail(`${l}: ${k} is a plural in one language and not the other`);
  }
  if (l !== SOURCE) for (const k of en.keys()) if (!c.has(k)) fail(`${l}: lacks ${k}`);
}

// ---- the modules, as tokens
/** Strings, templates (their fixed parts), punctuation and names, with comments and regexes left out. */
function lex(src) {
  const out = [];
  let i = 0, line = 1;
  const n = src.length;
  const modes = [];       // for each open template ${: the brace depth in it
  let depth = 0;
  const prev = () => out[out.length - 1];
  const regexAllowed = () => { const p = prev(); return !p || (p.type === 'punct' && !')]}'.includes(p.value)) || (p.type === 'name' && ['return', 'typeof', 'case', 'in', 'of', 'new', 'delete', 'void', 'throw'].includes(p.value)); };
  function template(tok) {
    // reads a template's fixed text from i until it ends (true) or opens a ${ (false)
    let s = '';
    while (i < n) {
      const c = src[i];
      if (c === '\\') { s += src[i + 1]; i += 2; continue; }
      if (c === '\n') line++;
      if (c === '`') { i++; tok.quasis.push(s); return true; }
      if (c === '$' && src[i + 1] === '{') { i += 2; tok.quasis.push(s); return false; }
      s += c; i++;
    }
    tok.quasis.push(s);
    return true;
  }
  const tpls = [];
  while (i < n) {
    const c = src[i];
    if (c === '\n') { line++; i++; continue; }
    if (/\s/.test(c)) { i++; continue; }
    if (c === '/' && src[i + 1] === '/') { while (i < n && src[i] !== '\n') i++; continue; }
    if (c === '/' && src[i + 1] === '*') { const j = src.indexOf('*/', i + 2); for (let k = i; k < (j < 0 ? n : j); k++) if (src[k] === '\n') line++; i = j < 0 ? n : j + 2; continue; }
    if (c === '"' || c === "'") {
      let j = i + 1, s = '';
      while (j < n && src[j] !== c) { if (src[j] === '\\') { s += src[j + 1]; j += 2; } else s += src[j++]; }
      out.push({ type: 'str', value: s, line }); i = j + 1; continue;
    }
    if (c === '`') {
      const tok = { type: 'tpl', quasis: [], line };
      out.push(tok); i++;
      if (!template(tok)) { modes.push(depth); tpls.push(tok); depth = 0; }
      continue;
    }
    if (c === '{') { depth++; out.push({ type: 'punct', value: c, line }); i++; continue; }
    if (c === '}') {
      if (modes.length && depth === 0) {
        i++;
        const tok = tpls[tpls.length - 1];
        if (template(tok)) { depth = modes.pop(); tpls.pop(); out.push({ type: 'punct', value: '`end', line }); }
        continue;
      }
      depth--; out.push({ type: 'punct', value: c, line }); i++; continue;
    }
    if (c === '/' && regexAllowed()) {
      let j = i + 1, cls = false;
      while (j < n && (cls || src[j] !== '/') && src[j] !== '\n') { if (src[j] === '\\') j++; else if (src[j] === '[') cls = true; else if (src[j] === ']') cls = false; j++; }
      i = j + 1; while (/[a-z]/.test(src[i] || '')) i++;
      out.push({ type: 'regex', line }); continue;
    }
    const m = /^[A-Za-z_$][\w$]*/.exec(src.slice(i, i + 64));
    if (m) { out.push({ type: 'name', value: m[0], line }); i += m[0].length; continue; }
    const num = /^\d[\d._a-fA-FxXnoObB]*/.exec(src.slice(i, i + 64));
    if (num) { out.push({ type: 'num', line }); i += num[0].length; continue; }
    const op = /^(===|!==|=>|\?\.|\?\?|&&|\|\||\.\.\.|[=!<>]=|[^\w\s])/.exec(src.slice(i, i + 3));
    out.push({ type: 'punct', value: op[0], line }); i += op[0].length;
  }
  return out;
}

function files(dir) {
  return readdirSync(dir).flatMap((f) => { const p = join(dir, f); return statSync(p).isDirectory() ? files(p) : p.endsWith('.js') ? [p] : []; });
}

// Where a string is not words: the value of these keys, and the first argument of these calls.
const DATA_KEYS = new Set(['class', 'id', 'type', 'variant', 'size', 'tone', 'href', 'autocomplete', 'inputMode', 'language', 'method', 'role',
  'rel', 'target', 'key', 'icon', 'value', 'for', 'htmlFor', 'labelledby', 'accept', 'mode', 'style', 'kind', 'pattern', 'preset', 'level',
  'status', 'state', 'action_key', 'cls', 'name_key', 'group_key', 'download', 'src', 'path', 'url', 'Image', 'Name', 'Driver', 'cmd', 'spellcheck']);
const SAY = new Set(['t', 'tOr', 'tParts', 'say']);
const DATA_CALLS = new Set(['h', 's', 'icon', 'querySelector', 'querySelectorAll', 'closest', 'matches', 'addEventListener', 'removeEventListener',
  'setAttribute', 'getAttribute', 'removeAttribute', 'hasAttribute', 'getItem', 'setItem', 'removeItem', 'get', 'post', 'put', 'del', 'api', 'base',
  'sbase', 'fetch', 'socket', 'postStream', 'RegExp', 'warn', 'error', 'log', 'createElement', 'createElementNS', 'setProperty', 'getPropertyValue',
  'split', 'join', 'replace', 'startsWith', 'endsWith', 'includes', 'indexOf', 'padStart', 'route', 'navigate', 'toggle', 'add', 'remove', 'contains',
  'dispatchEvent', 'Event', 'CustomEvent', 'postMessage', 'send', 'encodeURIComponent', 'set', 'has', 'delete', 'append', 'Blob', 'status', 'escape']);
// Proper names and Docker's own words that are the same in every language.
const NAMES = new Set(['Freightdock', 'Docker', 'Portainer', 'Compose', 'GitHub', 'GitLab', 'Google', 'Microsoft', 'Swarm', 'Authelia', 'Authentik',
  'Keycloak', 'OpenID', 'Cgroup', 'Gotify', 'Dockerfile', 'Git', 'JSON', 'YAML', 'IPv6', 'IPv4', 'Ctrl', 'Engine', 'Gitea', 'Forgejo', 'Bitbucket', 'Linux', 'Windows', 'CodeMirror', 'Basic', 'Bearer', 'Accept', 'Authorization', 'Content-Type']);
/** Strings that look like words and are on purpose not said through t(): file (under assets/js) and the text. */
const ON_PURPOSE = [
  // Compose's own lines, the same in every language.
  { file: 'security.js', text: /^(network_mode|ipc|uts): host\.$/ },
  // A Dockerfile to start from: Dockerfile's own words.
  { file: 'pages/images.js', text: /^FROM alpine/ },
  // A compose file to start from: Compose's own words.
  { file: 'pages/swarm-stacks.js', text: /^services: web: image: nginx/ },
  { file: 'pages/cluster-deploy.js', text: /^services: web: image: nginx/ },
  // The Compose lines the deployer writes for a volume on the cluster's storage.
  { file: 'compose-volumes.js', text: /x-freightdock: \{storage:|^, access: many$/ },
  // Docker's create body's own keys.
  { file: 'pages/container-edit.js', text: /^(Init|Tty|Privileged|Image|Cmd|Entrypoint|User|Hostname|Env|Labels|Binds|Memory)$/ },
  // Errors for whoever builds with a component, not for the person using the page.
  { file: 'components/code-editor/code-editor.js', text: /^code-editor: no language/ },
  { file: 'components/terminal/terminal.js', text: /did not load$/ },
  { file: 'components/qr-code/qr-encode.js', text: 'Too long for a QR code.' },
  // An example of the rules' own syntax, in a placeholder.
  { file: 'pages/signin-settings.js', text: /^devs -> Developers/ },
  // Product names and version numbers.
  { file: 'pages/signin-settings.js', text: /^(OpenID Connect|Microsoft Entra ID|Active Directory)$/ },
  { file: 'pages/account.js', text: /^(Debian, Ubuntu|Fedora, Arch|macOS|curl|Streamable HTTP)$/ },
  { file: 'pages/registries.js', text: /^(Docker Hub|GitHub \(ghcr\.io\)|Amazon ECR|Quay|GitLab)$/ },
  { file: 'pages/dashboard.js', text: 'Docker X · API X' },
];
const onPurpose = (file, raw) => { const text = raw.replace(/\s+/g, ' ').trim(); return ON_PURPOSE.some((o) => o.file === file && (o.text instanceof RegExp ? o.text.test(text) : o.text === text)); };

function looksLikeWords(text) {
  const s = text.replace(/\s+/g, ' ').trim();
  if (!s || s.length < 2) return false;
  if (/^-----BEGIN /.test(s)) return false;
  if (/@[a-z0-9-]+\.[a-z]/i.test(s) && !/\s[a-z]{3,}\s/.test(s.replace(/\S+@\S+/g, ''))) return false;   // e-mail addresses                         // a PEM's first line
  if (/^(\/|\.\/|\.\.\/|#|\.[a-z]|[a-z]+:\/\/|mailto:|data:|sha256:|\$\{)/.test(s) || s.includes('://')) return false;
  if (/^[a-z0-9_.:\-/[\]=@*+]+$/.test(s)) return false;           // an identifier, a class, a key, a path
  if (/^[A-Z0-9_\-./:+]+$/.test(s)) return false;                // CPU, GET, X-CSRF-Token-like codes
  const words = s.match(/[A-Za-zÀ-ÿ’']{2,}/g) || [];
  if (!words.length) return false;
  if (/\s/.test(s) && words.length >= 2) {
    // a list of classes, or a command line: not words
    if (/^[a-z][a-z0-9-]*( [a-z][a-z0-9-]*)*$/.test(s) && s.includes('-')) return false;
    if (/^(docker|curl|git|sh|cd|ls|cat|echo|apt|npm|cargo|sudo|claude|Import-Certificate|FREIGHTDOCK_|NODE_EXTRA_CA_CERTS=\S+|-v|-p|-e) /.test(s)) return false;
    return true;
  }
  if (words.length === 1 && /^[A-Z][a-z]{2,}[.!?…:]?$/.test(s) && !NAMES.has(s.replace(/[.!?…:]$/, ''))) return true;
  return false;
}

const used = new Set(), prefixes = new Set();
// What a module exports: one that imports a name its module does not export fails to load (node --check cannot see it),
// and since app.js imports every page, the whole app with it.
const exportsOf = new Map();
function exported(file) {
  if (!exportsOf.has(file)) {
    let src = '';
    try { src = readFileSync(file, 'utf8'); } catch { /* missing: nothing exported */ }
    const names = new Set([...src.matchAll(/^export (?:async )?(?:function\*?|const|let|var|class) ([A-Za-z_$][\w$]*)/gm)].map((m) => m[1]));
    for (const m of src.matchAll(/^export \{([^}]*)\}/gm)) for (const n of m[1].split(',')) { const parts = n.trim().split(/\s+as\s+/); if (parts[0]) names.add(parts[parts.length - 1]); }
    exportsOf.set(file, names);
  }
  return exportsOf.get(file);
}
for (const file of files(js)) {
  const rel = relative(js, file);
  // i18n.js is the words themselves; theme.js runs before any module, so before there are words.
  if (rel === 'i18n.js' || rel === 'theme.js') continue;
  const src = readFileSync(file, 'utf8');
  for (const m of src.matchAll(/^import \{([^}]*)\} from '(\.[^']+)';/gm)) {
    const target = join(dirname(file), m[2]);
    const names = exported(target);
    for (const name of m[1].split(',').map((x) => x.trim().split(/\s+as\s+/)[0]).filter(Boolean)) if (!names.has(name)) fail(`${rel}: ${m[2]} exports no ${name}`);
  }
  const toks = lex(src);
  for (let i = 0; i < toks.length; i++) {
    const tok = toks[i];
    // A component's `meta` is its documentation for people who build with it, in English like the code.
    if (tok.value === 'meta' && (toks[i - 1] || {}).value === 'const' && (toks[i + 2] || {}).value === '{') {
      let depth = 0, j = i + 2;
      for (; j < toks.length; j++) { if (toks[j].value === '{') depth++; else if (toks[j].value === '}' && --depth === 0) break; }
      i = j; continue;
    }
    // A string that is a key of the English catalogue is a key asked for, wherever it is (t(on ? 'a.b' : 'a.c')).
    if (tok.type === 'str' && en.has(tok.value)) { used.add(tok.value); continue; }
    if (tok.type === 'name' && SAY.has(tok.value) && tok.value !== 'say') {
      const p = toks[i - 1] || {}, nx = toks[i + 1] || {};
      const imported = toks.slice(Math.max(0, i - 12), i).some((x) => x.value === 'import') && toks.slice(i, i + 12).some((x) => x.value === 'from');
      if (nx.value !== '(' && p.value !== '.' && nx.value !== ':' && !imported) fail(`${rel}:${tok.line}: ${tok.value} names something else here; call it another name, t() is the words`);
    }
    if (tok.type !== 'str' && tok.type !== 'tpl') continue;
    const p1 = toks[i - 1] || {}, p2 = toks[i - 2] || {}, n1 = toks[i + 1] || {};
    // A key asked for.
    if (p1.value === '(' && p2.type === 'name' && SAY.has(p2.value) && (toks[i - 3] || {}).value !== '.') {
      if (tok.type === 'str') {
        used.add(tok.value);
        if (!en.has(tok.value)) fail(`${rel}:${tok.line}: no English text for ${tok.value}`);
      } else {
        const fixed = tok.quasis[0];
        prefixes.add(fixed);
        if (![...en.keys()].some((k) => k.startsWith(fixed))) fail(`${rel}:${tok.line}: no English text under ${fixed}…`);
      }
      continue;
    }
    // Not words: imports, object keys, the value of a key that holds data, a call that takes data, a comparison.
    if (p1.value === 'from' || p1.value === 'import' || (p1.value === '(' && p2.value === 'import')) continue;
    if (n1.value === ':' && (p1.value === '{' || p1.value === ',')) continue;
    if (p1.value === ':' && (p2.type === 'name' || p2.type === 'str') && (DATA_KEYS.has(p2.value) || /^data-|^aria-(controls|describedby|labelledby|current|hidden|live|expanded|selected|checked|haspopup|modal|valuenow)$/.test(p2.value))) continue;
    if (p1.value === '(' && p2.type === 'name' && DATA_CALLS.has(p2.value)) continue;
    if (['===', '!==', '==', '!=', 'case', 'in'].includes(p1.value) || ['===', '!==', '==', '!='].includes(n1.value)) continue;
    if (p1.value === '[' && n1.value === ']' && toks[i - 2] && (toks[i - 2].type === 'name' || toks[i - 2].value === ')' || toks[i - 2].value === ']')) continue;  // obj['key']
    // A template whose every fixed part is an address, a class or an id is not words.
    if (tok.type === 'tpl' && tok.quasis.every((q) => /^[\w.:\-/?=&#%@+,[\]]*$/.test(q))) continue;
    const text = tok.type === 'str' ? tok.value : tok.quasis.join(' X ');
    if (looksLikeWords(text) && !onPurpose(rel, text)) fail(`${rel}:${tok.line}: words not said through t(): ${JSON.stringify(text.length > 90 ? text.slice(0, 90) + '…' : text)}`);
  }
}
const covered = (k) => used.has(k) || [...prefixes].some((p) => p && k.startsWith(p)) || k.startsWith('finui.') || k.startsWith('units.');
for (const k of en.keys()) if (!covered(k)) fail(`${SOURCE}: ${k} is asked for by nothing`);

if (problems.length) {
  for (const p of problems) console.log(p);
  console.log(`${problems.length} problem${problems.length === 1 ? '' : 's'} with the UI's words`);
  process.exit(1);
}
console.log(`every word is in the catalogues: ${en.size} texts in ${LANGS.join(', ')}`);
