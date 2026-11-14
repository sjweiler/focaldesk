import test from 'node:test';
import assert from 'node:assert/strict';
import { colorHex,hexColor,switchPaint } from '../src/theme-editor.ts';
test('sRGB picker decodes gamma and round trips without changing alpha',()=>{
  const color=hexColor('#8080ff',.4);
  assert.ok(Math.abs(color.r-.21586)<.0001);
  assert.equal(colorHex(color),'#8080ff');
  assert.equal(color.a,.4);
});
test('gradient mode changes preserve independent stop colors and interpolation intent',()=>{
  const solid={mode:'solid',color:{space:'display_p3',r:1.1,g:-.05,b:.2,a:.6}};
  const linear=switchPaint(solid,'linear_gradient');
  assert.equal(linear.stops.length,2);
  linear.stops[1].color.r=.3;
  assert.equal(linear.stops[0].color.r,1.1);
  assert.equal(solid.color.r,1.1);
  linear.interpolation.space='display_p3';
  const radial=switchPaint(linear,'radial_gradient');
  assert.equal(radial.interpolation.space,'display_p3');
  assert.deepEqual(radial.stops,linear.stops);
  assert.deepEqual(switchPaint(radial,'solid').color,solid.color);
});
