// The deployer's Compose edits (web/assets/js/compose-volumes.js): a volume added on storage, and relative bind mounts
// turned into volumes, as text, so the file's comments and layout stay. Run by qa/js-check.sh (node --test).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { volumeName, serviceNames, declaredVolumes, addVolume, relativeBinds, convertBinds } from '../web/assets/js/compose-volumes.js';

const seq = (...xs) => () => xs.shift();

test('a volume name is the stack and a random part, never one the file has', () => {
  assert.equal(volumeName('ntfy', [], seq('k3f9q')), 'ntfy-data-k3f9q');
  assert.equal(volumeName('ntfy', ['ntfy-data-k3f9q'], seq('k3f9q', 'a1b2c')), 'ntfy-data-a1b2c');
  assert.equal(volumeName('', [], seq('x', 'zz')), 'stack-x-data-zz', 'no name yet: a random one');
  assert.equal(volumeName('Not A Name!', [], seq('x', 'zz')), 'stack-x-data-zz');
  assert.equal(volumeName('ntfy', [], seq('k3f9q'), 'config'), 'ntfy-config-k3f9q');
  assert.match(volumeName('ntfy', []), /^ntfy-data-[a-z0-9]{5}$/, 'random by default');
});

const NTFY = `services:
  ntfy:
    image: binwiederhier/ntfy
    # the data
    volumes:
      - ./:/var/lib/ntfy
      - "./config:/etc/ntfy:ro"
    command: serve
  web:
    image: nginx
    volumes: [./site:/usr/share/nginx/html, /srv/certs:/certs]
`;

test('the services are read from the file', () => {
  assert.deepEqual(serviceNames(NTFY), ['ntfy', 'web']);
  assert.deepEqual(serviceNames('services:\n    a:\n        image: x\n    "b":\n        image: y\nvolumes:\n  v: {}\n'), ['a', 'b']);
  assert.deepEqual(serviceNames('nothing: here\n'), []);
});

test('a volume on storage is declared, and mounted in the service asked', () => {
  const out = addVolume('services:\n  app:\n    image: x\n    ports: ["80:80"]\n', { name: 'v1', storage: 'ceph', service: 'app', target: '/data' });
  assert.equal(out, 'services:\n  app:\n    image: x\n    volumes:\n      - v1:/data\n    ports: ["80:80"]\nvolumes:\n  v1:\n    x-freightdock: {storage: ceph}\n');
});

test('a volume joins the lists already there, block or flow, and the volumes the file declares', () => {
  const file = 'services:\n  app:\n    image: x\n    volumes:\n    - a:/a\n    command: run\n  other:\n    image: y\n    volumes: [b:/b]\n\nvolumes:\n  a:\n  b: {}\n\n# the end\n';
  const one = addVolume(file, { name: 'c', storage: 'nas', service: 'app', target: '/c' });
  assert.equal(one, 'services:\n  app:\n    image: x\n    volumes:\n    - a:/a\n    - c:/c\n    command: run\n  other:\n    image: y\n    volumes: [b:/b]\n\nvolumes:\n  a:\n  b: {}\n  c:\n    x-freightdock: {storage: nas}\n\n# the end\n');
  const two = addVolume(one, { name: 'd', storage: null, service: 'other', target: '/d' });
  assert.match(two, /\n {4}volumes: \[b:\/b, d:\/d\]\n/);
  assert.match(two, /\n {2}d: \{\}\n/, 'a local volume: no storage');
});

test('a long-form volume list keeps its items whole', () => {
  const file = 'services:\n  app:\n    image: x\n    volumes:\n      - type: bind\n        source: /srv\n        target: /srv\n    restart: always\n';
  assert.equal(addVolume(file, { name: 'v', storage: 's', service: 'app', target: '/v' }),
    'services:\n  app:\n    image: x\n    volumes:\n      - type: bind\n        source: /srv\n        target: /srv\n      - v:/v\n    restart: always\nvolumes:\n  v:\n    x-freightdock: {storage: s}\n');
});

test('with no service named, only the declaration is added', () => {
  assert.equal(addVolume('services:\n  a:\n    image: x', { name: 'v', storage: 's' }), 'services:\n  a:\n    image: x\nvolumes:\n  v:\n    x-freightdock: {storage: s}\n');
});

test('a file that cannot be edited as text says so, in words the page translates', () => {
  const said = (f) => { try { f(); } catch (e) { return [e.key, e.vars]; } return null; };
  assert.deepEqual(said(() => addVolume('services: {a: {image: x}}\n', { name: 'v', storage: 's', service: 'a', target: '/v' })), ['services_flow', { service: 'a' }]);
  assert.deepEqual(said(() => addVolume('services:\n  a:\n    image: x\nvolumes: {v0: {}}\n', { name: 'v', storage: 's' })), ['volumes_flow', { name: 'v' }]);
  assert.deepEqual(said(() => addVolume(NTFY, { name: 'v', storage: 's', service: 'nope', target: '/v' })), ['no_service', { service: 'nope' }]);
  assert.deepEqual(said(() => addVolume('services:\n  a:\n    image: x\n    volumes: v:/v\n', { name: 'w', storage: 's', service: 'a', target: '/w' })), ['volumes_not_list', { service: 'a' }]);
});

test('the relative bind mounts are found, short and long form, and absolute ones left', () => {
  assert.deepEqual(relativeBinds(NTFY).map((b) => [b.service, b.source, b.target]), [['ntfy', './', '/var/lib/ntfy'], ['ntfy', './config', '/etc/ntfy'], ['web', './site', '/usr/share/nginx/html']]);
  const long = 'services:\n  a:\n    image: x\n    volumes:\n      - type: bind\n        source: ~/x\n        target: /x\n';
  assert.deepEqual(relativeBinds(long).map((b) => b.source), ['~/x']);
});

test('relative bind mounts become volumes on the storage, named for what they held', () => {
  const r = convertBinds(NTFY, { storage: 'ceph', stack: 'ntfy', rand: seq('aaaaa', 'bbbbb', 'ccccc') });
  assert.equal(r.text, `services:
  ntfy:
    image: binwiederhier/ntfy
    # the data
    volumes:
      - ntfy-data-aaaaa:/var/lib/ntfy
      - "ntfy-config-bbbbb:/etc/ntfy:ro"
    command: serve
  web:
    image: nginx
    volumes: [ntfy-site-ccccc:/usr/share/nginx/html, /srv/certs:/certs]
volumes:
  ntfy-data-aaaaa:
    x-freightdock: {storage: ceph}
  ntfy-config-bbbbb:
    x-freightdock: {storage: ceph}
  ntfy-site-ccccc:
    x-freightdock: {storage: ceph}
`);
  assert.deepEqual(r.changed.map((c) => `${c.source} > ${c.name}`), ['./ > ntfy-data-aaaaa', './config > ntfy-config-bbbbb', './site > ntfy-site-ccccc']);
});

test('one path shared by two services is one volume, many may use it', () => {
  const f = 'services:\n  a:\n    image: x\n    volumes: [./shared:/s]\n  b:\n    image: y\n    volumes:\n      - ./shared/:/t\n';
  const r = convertBinds(f, { storage: 'nas', stack: 'app', rand: seq('q1q1q') });
  assert.equal(r.text, 'services:\n  a:\n    image: x\n    volumes: [app-shared-q1q1q:/s]\n  b:\n    image: y\n    volumes:\n      - app-shared-q1q1q:/t\nvolumes:\n  app-shared-q1q1q:\n    x-freightdock: {storage: nas, access: many}\n');
});

test('a long-form bind becomes a volume', () => {
  const f = 'services:\n  a:\n    image: x\n    volumes:\n      - type: bind\n        source: ./db\n        target: /db\n';
  const r = convertBinds(f, { storage: 's', stack: 'a', rand: seq('zzzzz') });
  assert.equal(r.text, 'services:\n  a:\n    image: x\n    volumes:\n      - type: volume\n        source: a-db-zzzzz\n        target: /db\nvolumes:\n  a-db-zzzzz:\n    x-freightdock: {storage: s}\n');
});

test('nothing relative, nothing changed', () => {
  const f = 'services:\n  a:\n    image: x\n    volumes: [/srv:/srv, v:/v]\n';
  assert.deepEqual(convertBinds(f, { storage: 's', stack: 'a' }), { text: f, changed: [] });
});

test('the volumes the file declares are read, and none from elsewhere', () => {
  assert.deepEqual(declaredVolumes('services:\n  a:\n    image: x\nvolumes:\n  v1:\n    x-freightdock: {storage: s}\n  v2: {}\nnetworks:\n  n1:\n'), ['v1', 'v2']);
  assert.deepEqual(declaredVolumes('services:\n  a:\n    image: x\n'), []);
});
