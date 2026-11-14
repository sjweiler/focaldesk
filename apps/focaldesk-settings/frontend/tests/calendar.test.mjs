import test from 'node:test';
import assert from 'node:assert/strict';
import { eventTime,sameSelection } from '../src/calendar.ts';
test('all-day events keep their provider date independently of local timezone',()=>{
  assert.match(eventTime({all_day:true,start_time:'2026-11-12T00:00:00Z'},'en-US'),/Nov 12.*All day/);
  assert.equal(eventTime({all_day:false,start_time:'invalid',end_time:''}),'invalid');
});
test('calendar selection compares sets without treating order changes as edits',()=>{
  assert.equal(sameSelection(['primary','work'],['work','primary']),true);
  assert.equal(sameSelection(['primary'],['primary','work']),false);
});
