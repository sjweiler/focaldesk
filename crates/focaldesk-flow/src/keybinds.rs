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
                sym: keysyms::KEY_Escape,
            },
            KeyAction::ForceExit,
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
        let sym = keysym_to_lower(sym);
        let combo = KeyCombo { mods, sym };
        self.map.get(&combo).copied()
    }
}

fn keysym_to_lower(sym: u32) -> u32 {
    let utf32 = xkb::keysym_to_utf32(sym.into());
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
