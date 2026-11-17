// Exposures with "both" (web/assets/js/exposures.js): one row in the editor for a port on tcp and udp, two rules for
// the managers. Run by qa/js-check.sh (node --test).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mergeBoth, splitBoth, covers } from '../web/assets/js/exposures.js';

const e = (protocol, port = 53, extra = {}) => ({ service: 'dns', container_port: port, protocol, address: '192.0.2.10', port, ...extra });

test('a port exposed on tcp and on udp alike is one row, both', () => {
  assert.deepEqual(mergeBoth([e('tcp'), e('udp'), e('tcp', 80)]), [e('both'), e('tcp', 80)]);
});

test('only rules alike in all but the protocol are one', () => {
  const other = [e('tcp'), e('udp', 53, { address: '192.0.2.11' })];
  assert.deepEqual(mergeBoth(other), other);
  assert.deepEqual(mergeBoth([e('udp'), e('udp')]), [e('udp'), e('udp')], 'two of one protocol stay two');
});

test('both is two rules again for the managers, in the order written', () => {
  assert.deepEqual(splitBoth([e('both'), e('udp', 51820)]), [e('tcp'), e('udp'), e('udp', 51820)]);
  assert.deepEqual(splitBoth(mergeBoth([e('udp'), e('tcp')])).map((x) => x.protocol).sort(), ['tcp', 'udp']);
});

test('both covers tcp and udp', () => {
  assert.equal(covers('both', 'udp'), true);
  assert.equal(covers('tcp', 'udp'), false);
  assert.equal(covers('udp', 'udp'), true);
});
