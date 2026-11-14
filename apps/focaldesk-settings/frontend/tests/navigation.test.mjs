import test from 'node:test';
import assert from 'node:assert/strict';
import { sections, pageForPanel } from '../src/navigation.ts';

test('all familiar Settings sections retain their order with a combined keyboard and input destination', () => {
  assert.deepEqual(sections.map(section => section.name), [
    'Appearance', 'Accessibility', 'Theme Editor', 'Network', 'Calendar',
    'Bluetooth', 'Printers', 'Displays', 'Sound', 'Applications', 'Chrome',
    'Workspaces', 'Keyboard & Input', 'Privacy', 'Power', 'Debug', 'About',
  ]);
  assert.equal(new Set(sections.map(section => section.id)).size, 17);
  assert.ok(sections.every(section => section.page), 'all 17 sections have a native Tauri page');
  assert.equal(sections.find(section => section.name==='Keyboard & Input').page, 'input');
});

test('legacy panel arguments open their corresponding preview pages', () => {
  for (const section of sections) assert.equal(pageForPanel(section.id), section.page);
  assert.equal(pageForPanel('Theme Editor'), 'editor');
  assert.equal(pageForPanel('Mouse & Touchpad'), 'pointer');
  assert.equal(pageForPanel('Keyboard'), 'input');
  assert.equal(pageForPanel('Keyboard & Input'), 'input');
  assert.equal(pageForPanel('sound'), 'sound');
  assert.equal(pageForPanel('unknown'), 'appearance');
});
