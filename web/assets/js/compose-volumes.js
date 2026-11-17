// The deployer's Compose edits ("Cluster" 39): a named volume on the cluster's storage added to the file and mounted in
// a service, and the file's relative bind mounts (./data:/data, which mean nothing on a worker) turned into such volumes.
// Edited as text, line by line, so the file keeps its comments, order and layout; a part written on one line in flow
// style ({...}) is not taken apart, and the edit says to make it by hand. Tested by qa/compose-volumes.test.mjs.

const STACK = /^[a-z0-9][a-z0-9-]{0,39}$/;

/** An edit that cannot be made as text: `key` (under pages.cluster_deploy.vol_err) and its `vars`, for the page to
 *  say in the person's language. */
function refuse(key, vars) {
  return Object.assign(new Error(key), { key, vars });
}

/** Five random letters and digits. */
function random() {
  const b = new Uint8Array(5);
  globalThis.crypto.getRandomValues(b);
  return [...b].map((x) => '0123456789abcdefghijklmnopqrstuvwxyz'[x % 36]).join('');
}

/** What a name starts with: the stack's name when it is one, else a random one. */
function baseOf(stack, rand) {
  return STACK.test(stack || '') ? stack : `stack-${rand()}`;
}

function nameFrom(base, what, taken, rand) {
  for (;;) {
    const n = `${base}-${what}-${rand()}`;
    if (!taken.includes(n)) return n;
  }
}

/** A new volume's name: `<stack>-<what>-<random>`, one the file does not have (`taken`). The random part keeps it apart
 *  from what an earlier stack of the same name left on the storage too. */
export function volumeName(stack, taken = [], rand = random, what = 'data') {
  return nameFrom(baseOf(stack, rand), what, taken, rand);
}

const indentOf = (l) => l.match(/^ */)[0].length;
const isBlank = (l) => /^\s*(#.*)?$/.test(l);
const unquote = (s) => (/^(["']).*\1$/.test(s) ? s.slice(1, -1) : s);

/** A `key: rest` line: its indent, key (unquoted) and what follows the colon, its comment left off. */
function keyOf(l) {
  const m = l.match(/^( *)("[^"]*"|'[^']*'|[^\s#'"\-][^:#]*?|-(?=\S)[^:#]*?)\s*:(?:\s+(.*?))?\s*$/);
  if (!m) return null;
  const rest = (m[3] || '').replace(/(^|\s)#.*$/, '').trim();
  return { indent: m[1].length, key: unquote(m[2]), rest };
}

/** The top-level key `key`: its line and the end of its block (the next top-level line), or null. */
function topBlock(lines, key) {
  const at = lines.findIndex((l) => indentOf(l) === 0 && keyOf(l) && keyOf(l).key === key);
  if (at < 0) return null;
  let end = at + 1;
  while (end < lines.length && (isBlank(lines[end]) || indentOf(lines[end]) > 0)) end += 1;
  return { at, end, rest: keyOf(lines[at]).rest };
}

/** The keys right under the line `from`, up to `end`: each with its line and the end of its own block. */
function children(lines, from, end) {
  const inside = [];
  for (let i = from + 1; i < end; i += 1) if (!isBlank(lines[i])) inside.push(i);
  if (!inside.length) return [];
  const ind = Math.min(...inside.map((i) => indentOf(lines[i])));
  const keys = inside.filter((i) => indentOf(lines[i]) === ind && keyOf(lines[i]));
  return keys.map((i, n) => {
    let stop = n + 1 < keys.length ? keys[n + 1] : end;
    while (stop - 1 > i && isBlank(lines[stop - 1])) stop -= 1;
    return { at: i, end: stop, indent: ind, ...keyOf(lines[i]) };
  });
}

function services(lines) {
  const b = topBlock(lines, 'services');
  if (!b) return { list: [] };
  if (b.rest) return { list: [], flow: true };
  return { list: children(lines, b.at, b.end) };
}

/** The volumes the file declares at its top level, by name. */
export function declaredVolumes(text) {
  const lines = text.split('\n');
  const b = topBlock(lines, 'volumes');
  return b && !b.rest ? children(lines, b.at, b.end).map((c) => c.key) : [];
}

/** The services the file has, by name. */
export function serviceNames(text) {
  return services(text.split('\n')).list.map((s) => s.key);
}

function service(lines, name) {
  const s = services(lines);
  if (s.flow) throw refuse('services_flow', { service: name });
  const svc = s.list.find((x) => x.key === name);
  if (!svc) throw refuse('no_service', { service: name });
  return svc;
}

/** A block list's items under `at` (up to `end`): each item's first line and its end; and the list's indent. */
function items(lines, at, end) {
  let first = at + 1;
  while (first < end && isBlank(lines[first])) first += 1;
  if (first >= end || !/^ *- ?/.test(lines[first])) return { indent: null, list: [] };
  const ind = indentOf(lines[first]);
  const list = [];
  for (let i = first; i < end; i += 1) {
    const l = lines[i];
    if (isBlank(l)) continue;
    if (indentOf(l) < ind || (indentOf(l) === ind && !/^ *-( |$)/.test(l))) break;
    if (indentOf(l) === ind) list.push({ at: i, end: i + 1 });
    else list[list.length - 1].end = i + 1;
  }
  return { indent: ind, list };
}

/** The flow list `[a, b]` as its items. */
function flowItems(rest) {
  const inner = rest.slice(1, -1).trim();
  return inner ? inner.split(',').map((x) => x.trim()) : [];
}

/** Mounts `entry` in the service `name`: into its volumes list, block or flow, or a new one after its image. */
function mount(lines, name, entry) {
  const svc = service(lines, name);
  const props = children(lines, svc.at, svc.end);
  const vol = props.find((p) => p.key === 'volumes');
  if (!vol) {
    const ind = props.length ? props[0].indent : svc.indent + 2;
    const image = props.find((p) => p.key === 'image');
    const after = image ? image.at : svc.at;
    lines.splice(after + 1, 0, `${' '.repeat(ind)}volumes:`, `${' '.repeat(ind + 2)}- ${entry}`);
    return;
  }
  if (/^\[.*\]$/.test(vol.rest)) {
    const head = lines[vol.at].slice(0, lines[vol.at].indexOf('['));
    lines[vol.at] = `${head}[${[...flowItems(vol.rest), entry].join(', ')}]`;
    return;
  }
  if (vol.rest) throw refuse('volumes_not_list', { service: name });
  const it = items(lines, vol.at, vol.end);
  const after = it.list.length ? it.list[it.list.length - 1].end : vol.at + 1;
  lines.splice(after, 0, `${' '.repeat(it.indent ?? vol.indent + 2)}- ${entry}`);
}

/** Declares the volume `name` at the top level: on `storage` (`many` when several may use it), or local with none. */
function declare(lines, name, storage, many) {
  const b = topBlock(lines, 'volumes');
  let at, ind = 2;
  if (!b) {
    while (lines.length && lines[lines.length - 1] === '') lines.pop();
    lines.push('volumes:');
    at = lines.length;
  } else {
    if (b.rest === '{}') lines[b.at] = lines[b.at].replace(/\{\}\s*$/, '').trimEnd();
    else if (b.rest) throw refuse('volumes_flow', { name });
    const have = children(lines, b.at, b.end);
    if (have.some((v) => v.key === name)) return;
    if (have.length) ind = have[0].indent;
    at = have.length ? have[have.length - 1].end : b.at + 1;
  }
  const pad = ' '.repeat(ind);
  const decl = storage ? [`${pad}${name}:`, `${pad}  x-freightdock: {storage: ${storage}${many ? ', access: many' : ''}}`] : [`${pad}${name}: {}`];
  lines.splice(at, 0, ...decl);
  if (lines[lines.length - 1] !== '') lines.push('');
}

/** The file with the volume `name` declared (on `storage`, or local when it is null) and, when `service` is given,
 *  mounted there at `target`. */
export function addVolume(text, { name, storage, service: svc = null, target = null, many = false }) {
  const lines = text.split('\n');
  if (svc) mount(lines, svc, `${name}:${target}`);
  declare(lines, name, storage, many);
  return lines.join('\n');
}

const relative = (src) => src.startsWith('.') || src.startsWith('~');

/** The file's bind mounts of a relative path, in its services' volumes: where each is and how to rewrite it. */
export function relativeBinds(text) {
  const lines = text.split('\n');
  const out = [];
  const s = services(lines);
  for (const svc of s.list) {
    const vol = children(lines, svc.at, svc.end).find((p) => p.key === 'volumes');
    if (!vol) continue;
    if (/^\[.*\]$/.test(vol.rest)) {
      flowItems(vol.rest).forEach((raw, n) => {
        const [src, target] = unquote(raw).split(':');
        if (relative(src)) out.push({ service: svc.key, source: src, target, kind: 'flow', at: vol.at, n });
      });
      continue;
    }
    if (vol.rest) continue;
    for (const it of items(lines, vol.at, vol.end).list) {
      const first = lines[it.at];
      const content = first.replace(/^ *- ?/, '').trim();
      const k = keyOf(content);
      if (k && ['type', 'source', 'target', 'read_only', 'bind'].includes(k.key)) {
        const field = (key) => {
          const sub = [{ i: it.at, k }, ...Array.from({ length: it.end - it.at - 1 }, (_, j) => ({ i: it.at + 1 + j, k: keyOf(lines[it.at + 1 + j]) }))];
          return sub.find((x) => x.k && x.k.key === key);
        };
        const src = field('source'), dst = field('target'), type = field('type');
        if (src && relative(unquote(src.k.rest))) out.push({ service: svc.key, source: unquote(src.k.rest), target: dst ? unquote(dst.k.rest) : '', kind: 'long', at: src.i, type: type && type.k.rest === 'bind' ? type.i : null });
        continue;
      }
      const [src, target] = unquote(content).split(':');
      if (relative(src)) out.push({ service: svc.key, source: src, target, kind: 'short', at: it.at });
    }
  }
  return out;
}

/** A path as one folder: `./data/` and `./data` are the same. */
const folder = (src) => src.replace(/\/+$/, '') || src;

/** What a volume made from `src` is named for: its last folder, or `data`. */
function whatOf(src) {
  const last = folder(src).split('/').filter((p) => p && p !== '.' && p !== '..' && p !== '~').pop() || 'data';
  return last.toLowerCase().replace(/[^a-z0-9_.-]+/g, '-').replace(/^[-.]+|[-.]+$/g, '').slice(0, 24) || 'data';
}

/** The file with each relative bind mount made a named volume on `storage` (null: a local one), the same path one
 *  volume, used by many when several services share it; and what changed, each `{ source, name, services }`. */
export function convertBinds(text, { storage, stack, rand = random }) {
  const binds = relativeBinds(text);
  if (!binds.length) return { text, changed: [] };
  const lines = text.split('\n');
  const declared = declaredVolumes(text);
  const base = baseOf(stack, rand);
  const by = new Map();
  for (const b of binds) {
    const key = folder(b.source);
    if (!by.has(key)) by.set(key, { source: b.source, name: nameFrom(base, whatOf(b.source), [...declared, ...[...by.values()].map((x) => x.name)], rand), services: new Set() });
    by.get(key).services.add(b.service);
    b.name = by.get(key).name;
  }
  for (const b of binds) {
    const l = lines[b.at];
    if (b.kind === 'short') {
      const head = l.match(/^ *- ?\s*/)[0], content = l.slice(head.length);
      const q = /^["']/.test(content) ? content[0] : '';
      const inner = q ? content.slice(1, content.lastIndexOf(q)) : content;
      lines[b.at] = `${head}${q}${b.name}${inner.slice(b.source.length)}${q ? content.slice(content.lastIndexOf(q)) : ''}`;
    } else if (b.kind === 'long') {
      lines[b.at] = l.replace(/(source\s*:\s*)(["']?).*?\2(\s*(#.*)?)$/, `$1${b.name}$3`);
      if (b.type != null) lines[b.type] = lines[b.type].replace(/bind/, 'volume');
    }
  }
  // Flow lists, rebuilt once a line.
  for (const at of new Set(binds.filter((b) => b.kind === 'flow').map((b) => b.at))) {
    const l = lines[at], head = l.slice(0, l.indexOf('['));
    const list = flowItems(keyOf(l).rest).map((raw, n) => {
      const b = binds.find((x) => x.kind === 'flow' && x.at === at && x.n === n);
      if (!b) return raw;
      const q = /^["']/.test(raw) ? raw[0] : '';
      return `${q}${b.name}${unquote(raw).slice(b.source.length)}${q}`;
    });
    lines[at] = `${head}[${list.join(', ')}]`;
  }
  let out = lines.join('\n');
  for (const v of by.values()) out = addVolume(out, { name: v.name, storage, many: v.services.size > 1 });
  return { text: out, changed: [...by.values()].map((v) => ({ source: v.source, name: v.name, services: [...v.services] })) };
}
