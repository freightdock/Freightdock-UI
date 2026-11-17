// Which workers can keep a stack's data (web/assets/js/storage-fit.js): the deployer hides the rest. Run by
// qa/js-check.sh (node --test).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { unfit } from '../web/assets/js/storage-fit.js';

const ceph = { name: 'ceph', driver: 'cephfs', health: [{ node_id: 'old', ok: false, error: 'mount: permission denied' }, { node_id: 'host', ok: true }] };
const shared = { name: 'shared', driver: 'path', health: [] };
const nodes = [
  { id: 'host', report: { mounts: ['nfs', 'cephfs'] } },
  { id: 'lxc', report: { mounts: [], mounts_why: 'it runs in a user namespace (an unprivileged container)' } },
  { id: 'nocephclient', report: { mounts: ['nfs'] } },
  { id: 'old', report: {} },
  { id: 'quiet', report: {} },
];

test('a worker that says it cannot mount the storage is unfit, with its reason', () => {
  const u = unfit(nodes, [ceph], ['ceph']);
  assert.deepEqual([...u.keys()], ['lxc', 'nocephclient', 'old']);
  assert.deepEqual(u.get('lxc'), { storage: 'ceph', text: 'it runs in a user namespace (an unprivileged container)' });
  assert.deepEqual(u.get('nocephclient'), { storage: 'ceph', driver: 'cephfs' });
});

test('a worker too old to say is judged by its storage health, and one that says nothing is kept', () => {
  const u = unfit(nodes, [ceph], ['ceph']);
  assert.deepEqual(u.get('old'), { storage: 'ceph', text: 'mount: permission denied' });
  assert.equal(u.has('quiet'), false);
});

test('a path storage is never mounted, and a stack with no storage keeps every worker', () => {
  assert.equal(unfit(nodes, [ceph, shared], ['shared']).size, 0);
  assert.equal(unfit(nodes, [ceph], []).size, 0);
  assert.equal(unfit(nodes, [ceph], ['gone']).size, 0, 'a storage the cluster does not have is the managers to refuse');
});
