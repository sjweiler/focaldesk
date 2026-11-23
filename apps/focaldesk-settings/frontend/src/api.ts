import { invoke } from '@tauri-apps/api/core';

export interface Appearance {
  theme: string;
  font_scale: number;
  glow_strength: number;
  output_focus_glow: boolean;
  shader_chrome: boolean;
  work_area_glass: boolean;
  wallpaper_ai_hdr: boolean;
  rail_backdrop: 'solid' | 'smoked-glass';
  panel_darkness: number | null;
  task_shelf_visibility: 'always-visible' | 'intelligent-dodge' | 'autohide';
}
export interface ThemeChoice {
  id: string; name: string; source: string;
  accent: string; surface: string; text: string; dynamic_range: string;
  wallpaper_preview?: string; wallpaper_hdr?: boolean;
}
export interface Snapshot { appearance: Appearance; themes: ThemeChoice[] }
export interface About { version: string; operating_system: string; architecture: string; session: string }

export function sameAppearance(a: Appearance, b: Appearance): boolean {
  return a.theme === b.theme && a.font_scale === b.font_scale
    && a.glow_strength === b.glow_strength && a.output_focus_glow === b.output_focus_glow
    && a.shader_chrome === b.shader_chrome && a.work_area_glass === b.work_area_glass
    && a.wallpaper_ai_hdr === b.wallpaper_ai_hdr
    && a.rail_backdrop === b.rail_backdrop && a.panel_darkness === b.panel_darkness
    && a.task_shelf_visibility === b.task_shelf_visibility;
}
export const getAppearance = () => invoke<Snapshot>('get_appearance');
export const applyAppearance = (expected: Appearance, requested: Appearance) =>
  invoke<Snapshot>('apply_appearance', { expected, requested });
export const getAbout = () => invoke<About>('get_about');
export const openExistingSettings = (panel: string) => invoke<void>('open_existing_settings', { panel });

export type DisplayTransform = 'Normal' | 'Rotate90' | 'Rotate180' | 'Rotate270';
export interface HdrAppearance {black_level_nits:number;reference_white_nits:number;peak_nits:number;full_frame_peak_nits:number;saturation:number;midtone_gamma:number;tone_mapper:'bt2390'|'reinhard'|'hable'|'aces'}
export interface OutputConfig {
  connector: string; enabled: boolean; x: number; y: number;
  width: number; height: number; refresh_mhz: number; scale: number;
  transform: DisplayTransform; role: 'Normal' | 'ControlPanel'; primary: boolean;
  color_profile: 'auto' | 'srgb' | 'display_p3'; icc_profile_path: string | null;
  hdr_requested: boolean; hdr_enabled: boolean; hdr_appearance: HdrAppearance;
}
export interface DisplayMode { width: number; height: number; refresh_mhz: number }
export interface DisplayOutput {
  config: OutputConfig; make: string; model: string; modes: DisplayMode[];
  hdr_supported: boolean; hdr_active: boolean; wide_gamut_active: boolean; icc_lut_fallback_active: boolean;
}
export interface DisplayTrial { id: number; phase: 'applying' | 'confirming' | 'reverting'; remaining_ms: number }
export interface DisplayConfiguration { outputs: DisplayOutput[]; preview: DisplayTrial | null; hardware_modes: boolean }
export const getDisplays = () => invoke<DisplayConfiguration>('get_displays');
export const previewDisplays = (expected: OutputConfig[], outputs: OutputConfig[]) => invoke<DisplayConfiguration>('preview_displays', { expected, outputs });
export const confirmDisplays = (previewId: number) => invoke<DisplayConfiguration>('confirm_displays', { previewId });
export const revertDisplays = (previewId: number) => invoke<DisplayConfiguration>('revert_displays', { previewId });

export interface AudioDevice {
  id: string; index: number; name: string; label: string; kind: 'output' | 'input';
  volume_percent: number; muted: boolean; active_port: string | null; is_default: boolean;
}
export interface AudioSnapshot { server: string; outputs: AudioDevice[]; inputs: AudioDevice[] }
export type AudioAction = { type: 'set_default'; device_id: string }
  | { type: 'set_volume'; device_id: string; percent: number }
  | { type: 'set_mute'; device_id: string; muted: boolean };
export interface InputLevel { id: number; device_id: string; active: boolean; peak: number; rms: number; remaining_ms: number; error: string | null }
export const getAudio = () => invoke<AudioSnapshot>('get_audio');
export const applyAudio = (action: AudioAction) => invoke<AudioSnapshot>('apply_audio', { action });
export const startInputTest = (deviceId: string) => invoke<InputLevel>('start_input_test', { deviceId });
export const getInputLevel = (id: number) => invoke<InputLevel>('get_input_level', { id });
export const stopInputTest = (id: number) => invoke<void>('stop_input_test', { id });

export type AccelerationProfile = 'default' | 'adaptive' | 'flat';
export type ScrollMethod = 'default' | 'two_finger' | 'edge' | 'on_button_down' | 'disabled';
export interface InputDeviceSettings {
  enabled: boolean | null; tap_to_click: boolean | null; disable_while_typing: boolean | null;
  acceleration_profile: AccelerationProfile; acceleration_speed: number | null; scroll_method: ScrollMethod;
  left_handed: boolean | null; middle_button_emulation: boolean | null;
}
export interface InputSettings {
  pointer_speed: number; natural_scroll: boolean; workspace_swipe_fingers: number; workspace_swipe_distance: number;
  devices: Record<string, InputDeviceSettings>; keyboard_layout: string; keyboard_variant: string;
  keyboard_model: string; keyboard_options: string; keyboard_repeat_delay_ms: number; keyboard_repeat_rate: number;
  keybindings: Record<string, string>;
  accessibility: { sticky_keys: boolean; slow_keys: boolean; slow_keys_delay_ms: number; bounce_keys: boolean; bounce_keys_delay_ms: number; sticky_keys_shortcut: boolean; visual_feedback: boolean };
}
export interface InputDevice {
  id: string; name: string; sysname: string; vendor_id: number; product_id: number; touchpad: boolean;
  enabled: boolean; can_disable: boolean; tap_finger_count: number; tap_to_click: boolean;
  supports_disable_while_typing: boolean; disable_while_typing: boolean;
  supports_acceleration: boolean; supports_adaptive_acceleration: boolean; supports_flat_acceleration: boolean;
  acceleration_profile: string | null; acceleration_speed: number;
  supports_two_finger_scroll: boolean; supports_edge_scroll: boolean; supports_button_scroll: boolean;
  supports_disabled_scroll: boolean; scroll_method: string | null;
  supports_left_handed: boolean; left_handed: boolean; supports_middle_button_emulation: boolean; middle_button_emulation: boolean;
}
export interface InputShortcut { action: string; label: string; default: string }
export interface InputConfiguration { input: InputSettings; saved_input: InputSettings; devices: InputDevice[]; shortcuts: InputShortcut[]; layouts: { id: string; label: string }[]; hardware_devices: boolean }
export const getInputConfiguration = () => invoke<InputConfiguration>('get_input_configuration');
export const applyInputConfiguration = (expected: InputSettings, expectedSaved: InputSettings, input: InputSettings) => invoke<InputConfiguration>('apply_input_configuration', { expected, expectedSaved, input });
