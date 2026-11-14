export type Page = 'appearance' | 'editor' | 'calendar' | 'printers' | 'applications' | 'chrome' | 'workspaces' | 'debug' | 'accessibility' | 'network' | 'bluetooth' | 'displays' | 'sound' | 'input' | 'pointer' | 'privacy' | 'power' | 'about';
export type SettingsSection = { id: string; name: string; icon: string; page?: Page };

// Match the familiar Settings order while sections migrate independently.
export const sections: SettingsSection[] = [
  { id: 'appearance', name: 'Appearance', icon: '◒', page: 'appearance' },
  { id: 'accessibility', name: 'Accessibility', icon: '♿', page: 'accessibility' },
  { id: 'theme-editor', name: 'Theme Editor', icon: '◐', page: 'editor' },
  { id: 'network', name: 'Network', icon: '⌁', page: 'network' },
  { id: 'calendar', name: 'Calendar', icon: '▦', page: 'calendar' },
  { id: 'bluetooth', name: 'Bluetooth', icon: '⋈', page: 'bluetooth' },
  { id: 'printers', name: 'Printers', icon: '▤', page: 'printers' },
  { id: 'displays', name: 'Displays', icon: '▣', page: 'displays' },
  { id: 'audio', name: 'Sound', icon: '♫', page: 'sound' },
  { id: 'applications', name: 'Applications', icon: '⊞', page: 'applications' },
  { id: 'chrome', name: 'Chrome', icon: '▥', page: 'chrome' },
  { id: 'workspaces', name: 'Workspaces', icon: '▧', page: 'workspaces' },
  { id: 'keyboard-input', name: 'Keyboard & Input', icon: '⌨', page: 'input' },
  { id: 'privacy', name: 'Privacy', icon: '◇', page: 'privacy' },
  { id: 'power', name: 'Power', icon: '⏻', page: 'power' },
  { id: 'debug', name: 'Debug', icon: '⚙', page: 'debug' },
  { id: 'about', name: 'About', icon: 'ⓘ', page: 'about' },
];

export function pageForPanel(panel: string): Page {
  const normalized = panel.trim().toLowerCase();
  if (['mouse & touchpad', 'mouse-touchpad', 'pointer'].includes(normalized)) return 'pointer';
  if (['keyboard', 'input', 'keyboard & input', 'keyboard-input'].includes(normalized)) return 'input';
  const aliases: Record<string, string> = { 'theme editor': 'theme-editor', sound: 'audio' };
  return sections.find(section => section.id === (aliases[normalized] ?? normalized))?.page ?? 'appearance';
}
