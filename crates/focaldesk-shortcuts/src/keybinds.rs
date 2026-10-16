use crate::KeyAction;
use bitflags::bitflags;
use std::collections::HashMap;
use xkbcommon::xkb::{self, keysyms};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct ModMask: u32 {
        const SHIFT = 0b0001;
        const CTRL  = 0b0010;
        const ALT   = 0b0100;
        const SUPER = 0b1000;
    }
}

/// An XKB key symbol, distinct from a hardware keycode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySymbol(u32);

impl KeySymbol {
    /// Wrap a raw XKB keysym at a backend or parser boundary.
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    /// Return the raw XKB representation for diagnostics or backend interop.
    pub const fn raw(self) -> u32 {
        self.0
    }

    fn normalized(self) -> Self {
        if self.0 == keysyms::KEY_ISO_Left_Tab {
            return Self(keysyms::KEY_Tab);
        }

        let utf32 = xkb::keysym_to_utf32(self.0.into());
        if utf32 == 0 {
            return self;
        }
        let Some(ch) = char::from_u32(utf32) else {
            return self;
        };

        let mut lower = ch.to_lowercase();
        let Some(ch) = lower.next() else {
            return self;
        };
        if lower.next().is_some() {
            return self;
        }

        Self(xkb::utf32_to_keysym(ch as u32).raw())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyCombo {
    mods: ModMask,
    sym: KeySymbol,
}

impl KeyCombo {
    pub fn new(mods: ModMask, sym: KeySymbol) -> Self {
        Self {
            mods,
            sym: sym.normalized(),
        }
    }

    pub const fn modifiers(self) -> ModMask {
        self.mods
    }

    pub const fn symbol(self) -> KeySymbol {
        self.sym
    }

    fn normalized(self) -> Self {
        Self::new(self.mods, self.sym)
    }
}

#[derive(Clone, Default)]
struct BindingMap {
    inner: HashMap<KeyCombo, KeyAction>,
}

impl BindingMap {
    fn insert(&mut self, combo: KeyCombo, action: KeyAction) -> Option<KeyAction> {
        self.inner.insert(combo.normalized(), action)
    }

    fn get(&self, combo: &KeyCombo) -> Option<&KeyAction> {
        self.inner.get(combo)
    }

    fn retain(&mut self, predicate: impl FnMut(&KeyCombo, &mut KeyAction) -> bool) {
        self.inner.retain(predicate);
    }

    fn iter(&self) -> impl ExactSizeIterator<Item = (&KeyCombo, &KeyAction)> {
        self.inner.iter()
    }
}

/// Optional shortcut facilities provided by the embedding compositor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShortcutCapabilities {
    /// Reserve system-session keys such as Super+Space for the desktop shell.
    pub global_launcher: bool,
    /// Bind Print Screen shortcuts to the compositor's capture implementation.
    pub screen_capture: bool,
}

impl ShortcutCapabilities {
    /// Conservative bindings for a compositor nested inside another desktop.
    pub const NESTED: Self = Self {
        global_launcher: false,
        screen_capture: false,
    };

    /// Full bindings for a compositor that owns the desktop session.
    pub const DESKTOP: Self = Self {
        global_launcher: true,
        screen_capture: true,
    };
}

#[derive(Clone)]
pub struct Keybinds {
    map: BindingMap,
}

impl Keybinds {
    pub fn new() -> Self {
        Self {
            map: BindingMap::default(),
        }
    }

    pub fn with_defaults(capabilities: ShortcutCapabilities) -> Self {
        let mut kb = Self::new();

        kb.common_keybindings();
        kb.capability_keybindings(capabilities);

        kb
    }

    fn common_keybindings(&mut self) {
        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_Return),
            },
            KeyAction::LaunchTerminal, // or whatever you call it
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_b),
            },
            KeyAction::LaunchBrowser,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::CTRL | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_a),
            },
            KeyAction::LaunchAiConsole,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_q),
            },
            KeyAction::CloseFocused,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_l),
            },
            KeyAction::LockScreen,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_v),
            },
            KeyAction::ToggleClipboardHistory,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_v),
            },
            KeyAction::ToggleVoiceCapture,
        );

        for (mods, sym) in [
            (ModMask::SUPER, keysyms::KEY_equal),
            (ModMask::SUPER | ModMask::SHIFT, keysyms::KEY_plus),
            (ModMask::SUPER, keysyms::KEY_KP_Add),
        ] {
            self.map.insert(
                KeyCombo {
                    mods,
                    sym: KeySymbol::from_raw(sym),
                },
                KeyAction::MagnifierZoomIn,
            );
        }
        for sym in [keysyms::KEY_minus, keysyms::KEY_KP_Subtract] {
            self.map.insert(
                KeyCombo {
                    mods: ModMask::SUPER,
                    sym: KeySymbol::from_raw(sym),
                },
                KeyAction::MagnifierZoomOut,
            );
        }

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_N),
            },
            KeyAction::ToggleNightLight,
        );
        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_C),
            },
            KeyAction::CycleColorFilter,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_q),
            },
            KeyAction::QuitCompositor,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_F8),
            },
            KeyAction::FocusNext,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_F7),
            },
            KeyAction::FocusPrev,
        );
        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_Tab),
            },
            KeyAction::FocusNext,
        );
        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_Tab),
            },
            KeyAction::FocusPrev,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_Left),
            },
            KeyAction::SplitLeft,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_Right),
            },
            KeyAction::SplitRight,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_Up),
            },
            KeyAction::SplitTop,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_Down),
            },
            KeyAction::SplitBottom,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: KeySymbol::from_raw(keysyms::KEY_z),
            },
            KeyAction::ToggleSplitLayout,
        );

        for (sym, action) in [
            (keysyms::KEY_Left, KeyAction::SwapSplitLeft),
            (keysyms::KEY_Right, KeyAction::SwapSplitRight),
            (keysyms::KEY_Up, KeyAction::SwapSplitTop),
            (keysyms::KEY_Down, KeyAction::SwapSplitBottom),
        ] {
            self.map.insert(
                KeyCombo {
                    mods: ModMask::SUPER | ModMask::SHIFT,
                    sym: KeySymbol::from_raw(sym),
                },
                action,
            );
        }

        for (sym, action, fine_action) in [
            (
                keysyms::KEY_Left,
                KeyAction::ResizeSplitLeft,
                KeyAction::ResizeSplitLeftFine,
            ),
            (
                keysyms::KEY_Right,
                KeyAction::ResizeSplitRight,
                KeyAction::ResizeSplitRightFine,
            ),
            (
                keysyms::KEY_Up,
                KeyAction::ResizeSplitTop,
                KeyAction::ResizeSplitTopFine,
            ),
            (
                keysyms::KEY_Down,
                KeyAction::ResizeSplitBottom,
                KeyAction::ResizeSplitBottomFine,
            ),
        ] {
            self.map.insert(
                KeyCombo {
                    mods: ModMask::SUPER | ModMask::ALT,
                    sym: KeySymbol::from_raw(sym),
                },
                action,
            );
            self.map.insert(
                KeyCombo {
                    mods: ModMask::SUPER | ModMask::ALT | ModMask::SHIFT,
                    sym: KeySymbol::from_raw(sym),
                },
                fine_action,
            );
        }

        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_Tab),
            },
            KeyAction::FocusSplitNext,
        );
        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_Tab),
            },
            KeyAction::FocusSplitPrevious,
        );
        self.map.insert(
            KeyCombo {
                mods: ModMask::SUPER | ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_BackSpace),
            },
            KeyAction::UndoSplitAction,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::CTRL | ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_Tab),
            },
            KeyAction::FocusShellNext,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::CTRL | ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_Tab),
            },
            KeyAction::FocusShellPrevious,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::CTRL | ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_d),
            },
            KeyAction::ToggleLauncher,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_0),
            },
            KeyAction::OverflowView,
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_1),
            },
            KeyAction::ActivateSlot(0),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_2),
            },
            KeyAction::ActivateSlot(1),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_3),
            },
            KeyAction::ActivateSlot(2),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_4),
            },
            KeyAction::ActivateSlot(3),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_5),
            },
            KeyAction::ActivateSlot(4),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_6),
            },
            KeyAction::ActivateSlot(5),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_7),
            },
            KeyAction::ActivateSlot(6),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_8),
            },
            KeyAction::ActivateSlot(7),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT,
                sym: KeySymbol::from_raw(keysyms::KEY_9),
            },
            KeyAction::ActivateSlot(8),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_1),
            },
            KeyAction::AssignSlot(0),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_2),
            },
            KeyAction::AssignSlot(1),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_3),
            },
            KeyAction::AssignSlot(2),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_4),
            },
            KeyAction::AssignSlot(3),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_5),
            },
            KeyAction::AssignSlot(4),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_6),
            },
            KeyAction::AssignSlot(5),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_7),
            },
            KeyAction::AssignSlot(6),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_8),
            },
            KeyAction::AssignSlot(7),
        );

        self.map.insert(
            KeyCombo {
                mods: ModMask::ALT | ModMask::SHIFT,
                sym: KeySymbol::from_raw(keysyms::KEY_9),
            },
            KeyAction::AssignSlot(8),
        );
    }

    fn capability_keybindings(&mut self, capabilities: ShortcutCapabilities) {
        if capabilities.global_launcher {
            self.map.insert(
                KeyCombo {
                    mods: ModMask::SUPER,
                    sym: KeySymbol::from_raw(keysyms::KEY_space),
                },
                KeyAction::ToggleLauncher,
            );
        }

        if capabilities.screen_capture {
            self.map.insert(
                KeyCombo {
                    mods: ModMask::empty(),
                    sym: KeySymbol::from_raw(keysyms::KEY_Print),
                },
                KeyAction::TakeScreenshot,
            );

            self.map.insert(
                KeyCombo {
                    mods: ModMask::SHIFT,
                    sym: KeySymbol::from_raw(keysyms::KEY_Print),
                },
                KeyAction::TakeScreenshotAll,
            );
        }
    }

    pub fn resolve(&self, sym: KeySymbol, mods: ModMask) -> Option<KeyAction> {
        let combo = KeyCombo::new(mods, sym);
        self.map.get(&combo).copied()
    }

    /// Iterate over the active bindings without exposing mutable storage.
    pub fn bindings(&self) -> impl ExactSizeIterator<Item = (KeyCombo, KeyAction)> + '_ {
        self.map.iter().map(|(&combo, &action)| (combo, action))
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
        "launch_ai_console" | "launch_agent" => KeyAction::LaunchAiConsole,
        "lock_screen" => KeyAction::LockScreen,
        "take_screenshot" => KeyAction::TakeScreenshot,
        "take_screenshot_all" => KeyAction::TakeScreenshotAll,
        "launch_browser" => KeyAction::LaunchBrowser,
        "launch_files" => KeyAction::LaunchFiles,
        "toggle_clipboard_history" => KeyAction::ToggleClipboardHistory,
        "toggle_voice_capture" => KeyAction::ToggleVoiceCapture,
        "magnifier_zoom_in" => KeyAction::MagnifierZoomIn,
        "magnifier_zoom_out" => KeyAction::MagnifierZoomOut,
        "toggle_night_light" => KeyAction::ToggleNightLight,
        "cycle_color_filter" => KeyAction::CycleColorFilter,
        "split_left" => KeyAction::SplitLeft,
        "split_right" => KeyAction::SplitRight,
        "split_top" => KeyAction::SplitTop,
        "split_bottom" => KeyAction::SplitBottom,
        "toggle_split_layout" => KeyAction::ToggleSplitLayout,
        "swap_split_left" => KeyAction::SwapSplitLeft,
        "swap_split_right" => KeyAction::SwapSplitRight,
        "swap_split_top" => KeyAction::SwapSplitTop,
        "swap_split_bottom" => KeyAction::SwapSplitBottom,
        "resize_split_left" => KeyAction::ResizeSplitLeft,
        "resize_split_right" => KeyAction::ResizeSplitRight,
        "resize_split_top" => KeyAction::ResizeSplitTop,
        "resize_split_bottom" => KeyAction::ResizeSplitBottom,
        "resize_split_left_fine" => KeyAction::ResizeSplitLeftFine,
        "resize_split_right_fine" => KeyAction::ResizeSplitRightFine,
        "resize_split_top_fine" => KeyAction::ResizeSplitTopFine,
        "resize_split_bottom_fine" => KeyAction::ResizeSplitBottomFine,
        "focus_split_next" => KeyAction::FocusSplitNext,
        "focus_split_previous" | "focus_split_prev" => KeyAction::FocusSplitPrevious,
        "undo_split_action" => KeyAction::UndoSplitAction,
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

    Ok(KeyCombo::new(mods, KeySymbol::from_raw(sym)))
}

impl Default for Keybinds {
    fn default() -> Self {
        Self::with_defaults(ShortcutCapabilities::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(raw: u32) -> KeySymbol {
        KeySymbol::from_raw(raw)
    }

    #[test]
    fn key_symbol_round_trips_its_backend_representation() {
        let symbol = KeySymbol::from_raw(keysyms::KEY_F8);

        assert_eq!(symbol.raw(), keysyms::KEY_F8);
    }

    #[test]
    fn key_combo_constructor_normalizes_and_exposes_read_only_parts() {
        let combo = KeyCombo::new(ModMask::SUPER, key(keysyms::KEY_A));

        assert_eq!(combo.modifiers(), ModMask::SUPER);
        assert_eq!(combo.symbol().raw(), keysyms::KEY_a);
    }

    #[test]
    fn binding_storage_normalizes_internal_insertions() {
        let mut bindings = BindingMap::default();
        bindings.insert(
            KeyCombo {
                mods: ModMask::SUPER,
                sym: key(keysyms::KEY_A),
            },
            KeyAction::LaunchAiConsole,
        );

        assert_eq!(
            bindings
                .get(&KeyCombo::new(ModMask::SUPER, key(keysyms::KEY_a)))
                .copied(),
            Some(KeyAction::LaunchAiConsole)
        );
    }

    #[test]
    fn desktop_capabilities_include_common_and_system_bindings() {
        let keybinds = Keybinds::with_defaults(ShortcutCapabilities::DESKTOP);

        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Return), ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_space), ModMask::SUPER),
            Some(KeyAction::ToggleLauncher)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Print), ModMask::empty()),
            Some(KeyAction::TakeScreenshot)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Print), ModMask::SHIFT),
            Some(KeyAction::TakeScreenshotAll)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_F8), ModMask::SUPER),
            Some(KeyAction::FocusNext)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_equal), ModMask::SUPER),
            Some(KeyAction::MagnifierZoomIn)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_minus), ModMask::SUPER),
            Some(KeyAction::MagnifierZoomOut)
        );
    }

    #[test]
    fn nested_capabilities_exclude_system_session_bindings() {
        let keybinds = Keybinds::with_defaults(ShortcutCapabilities::NESTED);

        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Return), ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_space), ModMask::SUPER),
            None
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Print), ModMask::empty()),
            None
        );
    }

    #[test]
    fn optional_capabilities_are_independent() {
        let launcher_only = Keybinds::with_defaults(ShortcutCapabilities {
            global_launcher: true,
            screen_capture: false,
        });
        assert_eq!(
            launcher_only.resolve(key(keysyms::KEY_space), ModMask::SUPER),
            Some(KeyAction::ToggleLauncher)
        );
        assert_eq!(
            launcher_only.resolve(key(keysyms::KEY_Print), ModMask::empty()),
            None
        );

        let capture_only = Keybinds::with_defaults(ShortcutCapabilities {
            global_launcher: false,
            screen_capture: true,
        });
        assert_eq!(
            capture_only.resolve(key(keysyms::KEY_space), ModMask::SUPER),
            None
        );
        assert_eq!(
            capture_only.resolve(key(keysyms::KEY_Print), ModMask::empty()),
            Some(KeyAction::TakeScreenshot)
        );
    }

    #[test]
    fn shell_navigation_bindings_are_available_with_every_capability_set() {
        for capabilities in [ShortcutCapabilities::NESTED, ShortcutCapabilities::DESKTOP] {
            let keybinds = Keybinds::with_defaults(capabilities);
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_Tab), ModMask::CTRL | ModMask::ALT),
                Some(KeyAction::FocusShellNext)
            );
            assert_eq!(
                keybinds.resolve(
                    key(keysyms::KEY_ISO_Left_Tab),
                    ModMask::CTRL | ModMask::ALT | ModMask::SHIFT
                ),
                Some(KeyAction::FocusShellPrevious)
            );
        }
    }

    #[test]
    fn ai_console_binding_is_available_with_every_capability_set() {
        for capabilities in [ShortcutCapabilities::NESTED, ShortcutCapabilities::DESKTOP] {
            let keybinds = Keybinds::with_defaults(capabilities);
            assert_eq!(
                keybinds.resolve(
                    key(keysyms::KEY_a),
                    ModMask::SUPER | ModMask::CTRL | ModMask::SHIFT
                ),
                Some(KeyAction::LaunchAiConsole)
            );
        }
    }

    #[test]
    fn split_screen_bindings_are_available_with_every_capability_set() {
        for capabilities in [ShortcutCapabilities::NESTED, ShortcutCapabilities::DESKTOP] {
            let keybinds = Keybinds::with_defaults(capabilities);
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_Left), ModMask::SUPER),
                Some(KeyAction::SplitLeft)
            );
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_Down), ModMask::SUPER),
                Some(KeyAction::SplitBottom)
            );
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_z), ModMask::SUPER),
                Some(KeyAction::ToggleSplitLayout)
            );
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_Left), ModMask::SUPER | ModMask::SHIFT),
                Some(KeyAction::SwapSplitLeft)
            );
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_Right), ModMask::SUPER | ModMask::ALT),
                Some(KeyAction::ResizeSplitRight)
            );
            assert_eq!(
                keybinds.resolve(
                    key(keysyms::KEY_Down),
                    ModMask::SUPER | ModMask::ALT | ModMask::SHIFT
                ),
                Some(KeyAction::ResizeSplitBottomFine)
            );
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_Tab), ModMask::SUPER | ModMask::ALT),
                Some(KeyAction::FocusSplitNext)
            );
            assert_eq!(
                keybinds.resolve(key(keysyms::KEY_BackSpace), ModMask::SUPER | ModMask::ALT),
                Some(KeyAction::UndoSplitAction)
            );
        }
    }

    #[test]
    fn configurable_shortcuts_replace_defaults() {
        let mut keybinds = Keybinds::with_defaults(ShortcutCapabilities::DESKTOP);
        let warnings = keybinds.apply_overrides([("launch_terminal", "Ctrl+Alt+T")]);

        assert!(warnings.is_empty());
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_t), ModMask::CTRL | ModMask::ALT),
            Some(KeyAction::LaunchTerminal)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Return), ModMask::SUPER),
            None
        );
    }

    #[test]
    fn invalid_shortcuts_leave_the_default_binding_intact() {
        let mut keybinds = Keybinds::with_defaults(ShortcutCapabilities::NESTED);
        let warnings = keybinds.apply_overrides([("launch_terminal", "Hyper+DefinitelyNotAKey")]);

        assert_eq!(warnings.len(), 1);
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Return), ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
    }

    #[test]
    fn numbered_workspace_actions_are_configurable() {
        let mut keybinds = Keybinds::with_defaults(ShortcutCapabilities::NESTED);
        let warnings = keybinds.apply_overrides([
            ("activate_workspace_4", "Super+4"),
            ("move_to_workspace_4", "Super+Shift+4"),
        ]);

        assert!(warnings.is_empty());
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_4), ModMask::SUPER),
            Some(KeyAction::ActivateSlot(3))
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_4), ModMask::SUPER | ModMask::SHIFT),
            Some(KeyAction::AssignSlot(3))
        );
    }

    #[test]
    fn conflicting_override_does_not_displace_an_existing_action() {
        let mut keybinds = Keybinds::with_defaults(ShortcutCapabilities::NESTED);
        let warnings = keybinds.apply_overrides([("launch_terminal", "Super+B")]);

        assert_eq!(warnings.len(), 1);
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_b), ModMask::SUPER),
            Some(KeyAction::LaunchBrowser)
        );
        assert_eq!(
            keybinds.resolve(key(keysyms::KEY_Return), ModMask::SUPER),
            Some(KeyAction::LaunchTerminal)
        );
    }
}
