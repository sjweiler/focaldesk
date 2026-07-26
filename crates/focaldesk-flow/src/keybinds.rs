// all my keybind stuff goes here
use crate::actions::KeyAction;
use bitflags::bitflags;
use smithay::input::keyboard::{keysyms, xkb};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Winit,
    Drm,
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ModMask: u32 {
        const SHIFT = 0b0001;
        const CTRL  = 0b0010;
        const ALT   = 0b0100;
        const SUPER = 0b1000;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    pub mods: ModMask,
    pub sym: u32,
}

#[derive(Clone)]
pub struct Keybinds {
    pub map: HashMap<KeyCombo, KeyAction>,
}

impl Keybinds {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn with_defaults(backend: BackendKind) -> Self {
        let mut kb = Self::new();

        kb.common_keybindings();

        match backend {
            BackendKind::Winit => kb.winit_keybindings(),
            BackendKind::Drm => kb.drm_keybindings(),
        }

        kb
    }

    fn common_keybindings(&mut self) {
        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_Return,
            },
            KeyAction::LaunchTerminal, // or whatever you call it
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_b,
            },
            KeyAction::LaunchBrowser,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_q,
            },
            KeyAction::CloseFocused,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_l,
            },
            KeyAction::LockScreen,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_v,
            },
            KeyAction::ToggleClipboardHistory,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::SHIFT,
                sym: keysyms::KEY_v,
            },
            KeyAction::ToggleVoiceCapture,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::SHIFT,
                sym: keysyms::KEY_q,
            },
            KeyAction::QuitCompositor,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_F8,
            },
            KeyAction::FocusNext,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_F7,
            },
            KeyAction::FocusPrev,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::CTRL | ModMask::ALT,
                sym: keysyms::KEY_Tab,
            },
            KeyAction::FocusShellNext,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::CTRL | ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_Tab,
            },
            KeyAction::FocusShellPrevious,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::CTRL | ModMask::ALT,
                sym: keysyms::KEY_d,
            },
            KeyAction::ToggleLauncher,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_0,
            },
            KeyAction::OverflowView,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_1,
            },
            KeyAction::ActivateSlot(0),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_2,
            },
            KeyAction::ActivateSlot(1),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_3,
            },
            KeyAction::ActivateSlot(2),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_4,
            },
            KeyAction::ActivateSlot(3),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_5,
            },
            KeyAction::ActivateSlot(4),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_6,
            },
            KeyAction::ActivateSlot(5),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_7,
            },
            KeyAction::ActivateSlot(6),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_8,
            },
            KeyAction::ActivateSlot(7),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: keysyms::KEY_9,
            },
            KeyAction::ActivateSlot(8),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_1,
            },
            KeyAction::AssignSlot(0),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_2,
            },
            KeyAction::AssignSlot(1),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_3,
            },
            KeyAction::AssignSlot(2),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_4,
            },
            KeyAction::AssignSlot(3),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_5,
            },
            KeyAction::AssignSlot(4),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_6,
            },
            KeyAction::AssignSlot(5),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_7,
            },
            KeyAction::AssignSlot(6),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_8,
            },
            KeyAction::AssignSlot(7),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: keysyms::KEY_9,
            },
            KeyAction::AssignSlot(8),
        );
    }

    fn winit_keybindings(&mut self) {
        // Nested-only bindings go here later.
        // Example:
        // self.map.insert(
        //     KeyCombo { mods: ModMask::SUPER, sym: keysyms::KEY_F11 },
        //     KeyAction::ToggleFullscreen,
        // );
    }

    fn drm_keybindings(&mut self) {
        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: keysyms::KEY_space,
            },
            KeyAction::ToggleLauncher,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::empty(),
                sym: keysyms::KEY_Print,
            },
            KeyAction::TakeScreenshot,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SHIFT,
                sym: keysyms::KEY_Print,
            },
            KeyAction::TakeScreenshotAll,
        );
    }

    pub fn resolve(&self, sym: u32, mods: ModMask) -> Option<KeyAction> {
        // XKB commonly reports Shift+Tab as ISO_Left_Tab. Treat it as the same
        // physical navigation key and let the modifier mask select direction.
        let sym = if sym == keysyms::KEY_ISO_Left_Tab {
            keysyms::KEY_Tab
        } else {
            keysym_to_lower(sym)
        };
        let combo = KeyCombo { mods, sym };
        self.map.get(&combo).copied()
    }

    /// Apply user shortcut overrides while retaining defaults for invalid
    /// entries. Conflicts are rejected so one override cannot silently disable
    /// another action.
    pub fn apply_overrides<'a>(
        &mut self,
        overrides: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        for (action_name, shortcut) in overrides {
            let Some(action) = action_from_name(action_name) else {
                warnings.push(format!("unknown keybinding action `{action_name}`"));
                continue;
            };
            let combo = match parse_shortcut(shortcut) {
                Ok(combo) => combo,
                Err(error) => {
                    warnings.push(format!("invalid shortcut for `{action_name}`: {error}"));
                    continue;
                }
            };

            if let Some(displaced) = self.map.get(&combo).copied()
                && displaced != action
            {
                warnings.push(format!(
                    "shortcut `{shortcut}` for `{action_name}` conflicts with {displaced:?}"
                ));
                continue;
            }
            self.map.retain(|_, existing| *existing != action);
            self.map.insert(combo, action);
        }
        warnings
    }
}

fn action_from_name(name: &str) -> Option<KeyAction> {
    let normalized = name.trim().to_ascii_lowercase().replace('-', "_");
    let action = match normalized.as_str() {
        "close_focused" => KeyAction::CloseFocused,
        "focus_next" => KeyAction::FocusNext,
        "focus_previous" | "focus_prev" => KeyAction::FocusPrev,
        "focus_shell_next" => KeyAction::FocusShellNext,
        "focus_shell_previous" | "focus_shell_prev" => KeyAction::FocusShellPrevious,
        "show_workspaces" | "overflow_view" => KeyAction::OverflowView,
        "quit_compositor" => KeyAction::QuitCompositor,
        "toggle_launcher" => KeyAction::ToggleLauncher,
        "launch_terminal" => KeyAction::LaunchTerminal,
        "lock_screen" => KeyAction::LockScreen,
        "take_screenshot" => KeyAction::TakeScreenshot,
        "take_screenshot_all" => KeyAction::TakeScreenshotAll,
        "launch_browser" => KeyAction::LaunchBrowser,
        "launch_files" => KeyAction::LaunchFiles,
        "toggle_clipboard_history" => KeyAction::ToggleClipboardHistory,
        "toggle_voice_capture" => KeyAction::ToggleVoiceCapture,
        _ => {
            if let Some(slot) = numbered_action(&normalized, "activate_workspace_") {
                return Some(KeyAction::ActivateSlot(slot));
            }
            if let Some(slot) = numbered_action(&normalized, "move_to_workspace_") {
                return Some(KeyAction::AssignSlot(slot));
            }
            return None;
        }
    };
    Some(action)
}

fn numbered_action(name: &str, prefix: &str) -> Option<usize> {
    let number = name.strip_prefix(prefix)?.parse::<usize>().ok()?;
    (1..=9).contains(&number).then_some(number - 1)
}

fn parse_shortcut(shortcut: &str) -> Result<KeyCombo, String> {
    let parts: Vec<&str> = shortcut
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    let (key_name, modifiers) = parts
        .split_last()
        .ok_or_else(|| "shortcut is empty".to_string())?;

    let mut mods = ModMask::empty();
    for modifier in modifiers {
        let flag = match modifier.to_ascii_lowercase().as_str() {
            "shift" => ModMask::SHIFT,
            "ctrl" | "control" => ModMask::CTRL,
            "alt" => ModMask::ALT,
            "super" | "logo" | "meta" => ModMask::SUPER,
            _ => return Err(format!("unknown modifier `{modifier}`")),
        };
        if mods.contains(flag) {
            return Err(format!("duplicate modifier `{modifier}`"));
        }
        mods |= flag;
    }

    let canonical = match key_name.to_ascii_lowercase().as_str() {
        "enter" => "Return".to_string(),
        "esc" => "Escape".to_string(),
        "space" => "space".to_string(),
        "printscreen" => "Print".to_string(),
        other if other.len() == 1 => other.to_string(),
        _ => key_name.to_string(),
    };
    let sym = xkb::keysym_from_name(&canonical, 0).raw();
    if sym == keysyms::KEY_NoSymbol {
        return Err(format!("unknown key `{key_name}`"));
    }

    Ok(KeyCombo {
        mods,
        sym: keysym_to_lower(sym),
    })
}

fn keysym_to_lower(sym: u32) -> u32 {
    let utf32 = xkb::keysym_to_utf32(sym.into());
    if utf32 == 0 {
        return sym;
    }
    let Some(ch) = char::from_u32(utf32) else {
        return sym;
    };

    let mut lower = ch.to_lowercase();
    let Some(ch) = lower.next() else {
        return sym;
    };
    if lower.next().is_some() {
        return sym;
    }

    xkb::utf32_to_keysym(ch as u32).raw()
}

impl Default for Keybinds {
    fn default() -> Self {
        Self::with_defaults(BackendKind::Winit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drm_defaults_include_common_and_drm_bindings() {
        let keybinds = Keybinds::with_defaults(BackendKind::Drm);

        assert_eq!(
            keybinds.resolve(keysyms::KEY_Return, ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
        assert_eq!(
            keybinds.resolve(keysyms::KEY_space, ModMask::SUPER),
            Some(KeyAction::ToggleLauncher)
        );
        assert_eq!(
            keybinds.resolve(keysyms::KEY_Print, ModMask::empty()),
            Some(KeyAction::TakeScreenshot)
        );
        assert_eq!(
            keybinds.resolve(keysyms::KEY_Print, ModMask::SHIFT),
            Some(KeyAction::TakeScreenshotAll)
        );
        assert_eq!(
            keybinds.resolve(keysyms::KEY_F8, ModMask::SUPER),
            Some(KeyAction::FocusNext)
        );
    }

    #[test]
    fn winit_defaults_exclude_drm_only_bindings() {
        let keybinds = Keybinds::with_defaults(BackendKind::Winit);

        assert_eq!(
            keybinds.resolve(keysyms::KEY_Return, ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
        assert_eq!(keybinds.resolve(keysyms::KEY_space, ModMask::SUPER), None);
        assert_eq!(keybinds.resolve(keysyms::KEY_Print, ModMask::empty()), None);
    }

    #[test]
    fn shell_navigation_bindings_are_available_on_every_backend() {
        for backend in [BackendKind::Winit, BackendKind::Drm] {
            let keybinds = Keybinds::with_defaults(backend);
            assert_eq!(
                keybinds.resolve(keysyms::KEY_Tab, ModMask::CTRL | ModMask::ALT),
                Some(KeyAction::FocusShellNext)
            );
            assert_eq!(
                keybinds.resolve(
                    keysyms::KEY_ISO_Left_Tab,
                    ModMask::CTRL | ModMask::ALT | ModMask::SHIFT
                ),
                Some(KeyAction::FocusShellPrevious)
            );
        }
    }

    #[test]
    fn configurable_shortcuts_replace_defaults() {
        let mut keybinds = Keybinds::with_defaults(BackendKind::Drm);
        let warnings = keybinds.apply_overrides([("launch_terminal", "Ctrl+Alt+T")]);

        assert!(warnings.is_empty());
        assert_eq!(
            keybinds.resolve(keysyms::KEY_t, ModMask::CTRL | ModMask::ALT),
            Some(KeyAction::LaunchTerminal)
        );
        assert_eq!(keybinds.resolve(keysyms::KEY_Return, ModMask::SUPER), None);
    }

    #[test]
    fn invalid_shortcuts_leave_the_default_binding_intact() {
        let mut keybinds = Keybinds::with_defaults(BackendKind::Winit);
        let warnings = keybinds.apply_overrides([("launch_terminal", "Hyper+DefinitelyNotAKey")]);

        assert_eq!(warnings.len(), 1);
        assert_eq!(
            keybinds.resolve(keysyms::KEY_Return, ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
    }

    #[test]
    fn numbered_workspace_actions_are_configurable() {
        let mut keybinds = Keybinds::with_defaults(BackendKind::Winit);
        let warnings = keybinds.apply_overrides([
            ("activate_workspace_4", "Super+4"),
            ("move_to_workspace_4", "Super+Shift+4"),
        ]);

        assert!(warnings.is_empty());
        assert_eq!(
            keybinds.resolve(keysyms::KEY_4, ModMask::SUPER),
            Some(KeyAction::ActivateSlot(3))
        );
        assert_eq!(
            keybinds.resolve(keysyms::KEY_4, ModMask::SUPER | ModMask::SHIFT),
            Some(KeyAction::AssignSlot(3))
        );
    }

    #[test]
    fn conflicting_override_does_not_displace_an_existing_action() {
        let mut keybinds = Keybinds::with_defaults(BackendKind::Winit);
        let warnings = keybinds.apply_overrides([("launch_terminal", "Super+B")]);

        assert_eq!(warnings.len(), 1);
        assert_eq!(
            keybinds.resolve(keysyms::KEY_b, ModMask::SUPER),
            Some(KeyAction::LaunchBrowser)
        );
        assert_eq!(
            keybinds.resolve(keysyms::KEY_Return, ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
    }
}
