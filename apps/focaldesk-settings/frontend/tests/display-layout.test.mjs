import test from 'node:test';
import assert from 'node:assert/strict';
import { logicalSize, layoutBounds, snapPosition } from '../src/display-layout.ts';
const monitor = { connector: 'DP-1', width: 3840, height: 2160, scale: 2, transform: 'Normal', x: 0, y: 0 };
test('monitor arrangement uses rotated logical coordinates and includes negative origins', () => {
  assert.deepEqual(logicalSize(monitor), { width: 1920, height: 1080 });
  const portrait = { ...monitor, transform: 'Rotate90', x: -1080, y: -200 };
  assert.deepEqual(logicalSize(portrait), { width: 1080, height: 1920 });
  assert.deepEqual(layoutBounds([monitor, portrait]), { x: -1080, y: -200, width: 3000, height: 1920 });
});
test('dragging snaps neighboring edges and leaves distant positions untouched', () => {
  assert.deepEqual(snapPosition(monitor, [monitor], 1900, 12), { x: 1920, y: 0 });
  assert.deepEqual(snapPosition(monitor, [monitor], 4000, 4000), { x: 4000, y: 4000 });
});
