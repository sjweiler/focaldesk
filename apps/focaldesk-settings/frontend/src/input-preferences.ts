import type { InputDevice, InputDeviceSettings, InputSettings } from './api';
export function copy<T>(value: T): T { return JSON.parse(JSON.stringify(value)); }
export function sameInput(a: InputSettings, b: InputSettings): boolean {
  // Canonical key order prevents equivalent shortcut/device maps from appearing dirty.
  const canonical = (value: unknown): unknown => {
    if (Array.isArray(value)) return value.map(canonical);
    if (value && typeof value === 'object') return Object.fromEntries(Object.entries(value).sort(([a],[b]) => a.localeCompare(b)).map(([key,value]) => [key,canonical(value)]));
    return value;
  };
  return JSON.stringify(canonical(a)) === JSON.stringify(canonical(b));
}
export function currentDeviceSettings(device: InputDevice): InputDeviceSettings {
  return {
    enabled: null,
    tap_to_click: device.tap_finger_count > 0 ? device.tap_to_click : null,
    disable_while_typing: device.supports_disable_while_typing ? device.disable_while_typing : null,
    acceleration_profile: device.acceleration_profile === 'adaptive' || device.acceleration_profile === 'flat' ? device.acceleration_profile : 'default',
    acceleration_speed: device.supports_acceleration ? device.acceleration_speed : null,
    scroll_method: ['two_finger','edge','on_button_down','disabled'].includes(device.scroll_method ?? '') ? device.scroll_method as InputDeviceSettings['scroll_method'] : 'default',
    left_handed: device.supports_left_handed ? device.left_handed : null,
    middle_button_emulation: device.supports_middle_button_emulation ? device.middle_button_emulation : null,
  };
}

export function capturedShortcut(event: Pick<KeyboardEvent, 'key' | 'ctrlKey' | 'altKey' | 'shiftKey' | 'metaKey' | 'isComposing'>): string | null {
  if (event.isComposing || ['Control','Alt','Shift','Meta','Dead','Unidentified'].includes(event.key)) return null;
  const keys: Record<string, string> = { ' ': 'space', '+': 'plus', Enter: 'Return', Backspace: 'BackSpace', ArrowLeft: 'Left', ArrowRight: 'Right', ArrowUp: 'Up', ArrowDown: 'Down', Delete: 'Delete', PageUp: 'Page_Up', PageDown: 'Page_Down', PrintScreen: 'Print' };
  return [event.ctrlKey ? 'Ctrl' : '', event.altKey ? 'Alt' : '', event.shiftKey ? 'Shift' : '', event.metaKey ? 'Super' : '', keys[event.key] ?? event.key].filter(Boolean).join('+');
}
