// The Storage page's sums (web/assets/js/usage-sums.js). Run by qa/js-check.sh (node --test).

import { test } from 'node:test';
import assert from 'node:assert/strict';
import { totals, share } from '../web/assets/js/usage-sums.js';

test('the totals add up what is known and say how much is not', () => {
  const t = totals([{ total_bytes: 100, used_bytes: 40, free_bytes: 60 }, { total_bytes: 300, used_bytes: 100, free_bytes: 200 }, { total_bytes: null, used_bytes: null }]);
  assert.deepEqual(t, { total: 400, used: 140, free: 260, share: 0.35, unknown: 1 });
});

test('free is what is not used when not said', () => {
  assert.equal(totals([{ total_bytes: 10, used_bytes: 4 }]).free, 6);
});

test('nothing known is nothing, not a division by zero', () => {
  assert.deepEqual(totals([]), { total: 0, used: 0, free: 0, share: null, unknown: 0 });
});

test('a share of a total, or none without one', () => {
  assert.equal(share(25, 100), 0.25);
  assert.equal(share(null, 100), null);
  assert.equal(share(5, 0), null);
  assert.equal(share(5, null), null);
});
