import test from 'node:test';
import assert from 'node:assert/strict';
import { clockTime, clockMinutes } from '../src/accessibility.ts';

test('Night Light time values preserve midnight, overnight schedules, and minute precision', () => {
  for (const minutes of [0,1,419,1140,1439]) assert.equal(clockMinutes(clockTime(minutes)), minutes);
  assert.equal(clockTime(1140), '19:00');
  assert.equal(clockMinutes('07:00'), 420);
});
test('incomplete or invalid clock input never changes the schedule', () => {
  for (const value of ['', '24:00', '12:60', '-1:00', '7:00', '19:']) assert.equal(clockMinutes(value), null);
});
