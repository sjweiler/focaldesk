import test from 'node:test';
import assert from 'node:assert/strict';
import { sameAppearance } from '../src/api.ts';

test('dirty state compares the complete editable appearance without object identity', () => {
  const saved = { theme: 'Default', font_scale: 1, glow_strength: 0.75, output_focus_glow: true, shader_chrome:true, work_area_glass:true, rail_backdrop:'solid', panel_darkness:null, task_shelf_visibility:'autohide' };
  assert.equal(sameAppearance(saved, { ...saved }), true);
  for (const [key, value] of Object.entries({ theme: 'package:classic', font_scale: 1.25, glow_strength: 0, output_focus_glow: false, shader_chrome:false, work_area_glass:false, rail_backdrop:'smoked-glass', panel_darkness:0.5, task_shelf_visibility:'always-visible' })) {
    assert.equal(sameAppearance(saved, { ...saved, [key]: value }), false, key);
  }
});
