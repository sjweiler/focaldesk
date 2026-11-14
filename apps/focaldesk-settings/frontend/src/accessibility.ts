import type { InputSettings } from './api';
export interface VisualAccessibility {
  magnifier_enabled: boolean; magnifier_zoom: number; magnifier_follow_focus: boolean;
  magnifier_output_zoom: Record<string, number>; cursor_size: number;
  night_light: { enabled: boolean; temperature_kelvin: number; schedule: 'manual' | 'custom' | 'automatic'; custom_start_minutes: number; custom_end_minutes: number; transition_minutes: number; latitude: number | null; longitude: number | null };
  color_filter: 'off' | 'grayscale' | 'protanopia' | 'deuteranopia' | 'tritanopia';
  display_comfort_disabled_outputs: Record<string, boolean>;
  ambient_light_enabled: boolean; ambient_min_brightness: number; on_screen_keyboard_enabled: boolean;
}
export interface AccessibilityDraft { visual: VisualAccessibility; keyboard: InputSettings['accessibility'] }
export interface AccessibilitySnapshot { preferences: AccessibilityDraft; outputs: string[] }
export const clockTime = (minutes: number) => `${Math.floor(minutes / 60).toString().padStart(2,'0')}:${(minutes % 60).toString().padStart(2,'0')}`;
export function clockMinutes(value: string): number | null {
  if (!/^([01]\d|2[0-3]):[0-5]\d$/.test(value)) return null;
  const [hours,minutes] = value.split(':').map(Number);
  return hours*60+minutes;
}
