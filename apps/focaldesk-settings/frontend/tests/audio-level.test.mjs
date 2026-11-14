import test from 'node:test';
import assert from 'node:assert/strict';
import { meterPercent, volumeLabel } from '../src/audio-level.ts';
test('microphone meter has a silence floor, logarithmic range, and bounded clipping', () => {
  assert.equal(meterPercent(0), 0);
  assert.equal(meterPercent(0.00001), 0);
  assert.equal(meterPercent(NaN), 0);
  assert.equal(meterPercent(1), 100);
  assert.equal(meterPercent(2), 100);
  assert.equal(meterPercent(0.001), 0);
  assert.ok(Math.abs(meterPercent(0.1) - 66.66667) < 0.001);
  assert.equal(volumeLabel(49.9), '50%');
});
