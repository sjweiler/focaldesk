import test from 'node:test';
import assert from 'node:assert/strict';
import { copy, sameInput, currentDeviceSettings } from '../src/input-preferences.ts';

test('equivalent map order is clean while nested changes are dirty', () => {
  const before = { keybindings: { a: 'Super+A', b: 'Super+B' }, devices: { mouse: { acceleration_speed: 0.2 } } };
  const reordered = { devices: { mouse: { acceleration_speed: 0.2 } }, keybindings: { b: 'Super+B', a: 'Super+A' } };
  assert.equal(sameInput(before, reordered), true);
  const draft = copy(before);
  draft.devices.mouse.acceleration_speed = 0.3;
  assert.equal(sameInput(before, draft), false);
  assert.equal(before.devices.mouse.acceleration_speed, 0.2);
});

test('device edits start with actual supported values and preserve hardware enablement', () => {
  const settings = currentDeviceSettings({ tap_finger_count: 3, tap_to_click: true,
    supports_disable_while_typing: true, disable_while_typing: true,
    supports_acceleration: true, acceleration_profile: 'flat', acceleration_speed: -0.4,
    scroll_method: 'two_finger', supports_left_handed: false,
    supports_middle_button_emulation: false });
  assert.equal(settings.enabled, null);
  assert.equal(settings.tap_to_click, true);
  assert.equal(settings.acceleration_speed, -0.4);
  assert.equal(settings.acceleration_profile, 'flat');
  assert.equal(settings.scroll_method, 'two_finger');
  assert.equal(settings.left_handed, null);
});

import { capturedShortcut } from '../src/input-preferences.ts';
test('shortcut capture translates browser keys and ignores composition and bare modifiers', () => {
  const key = { key: 'ArrowLeft', ctrlKey: true, altKey: false, shiftKey: true, metaKey: false, isComposing: false };
  assert.equal(capturedShortcut(key), 'Ctrl+Shift+Left');
  assert.equal(capturedShortcut({...key, key:'+'}), 'Ctrl+Shift+plus');
  assert.equal(capturedShortcut({...key, key:'Shift'}), null);
  assert.equal(capturedShortcut({...key, isComposing:true}), null);
});
