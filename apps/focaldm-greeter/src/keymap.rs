// Minimal evdev-keycode keyboard handling for the greeter. This is a fixed
// US-QWERTY table, not full xkbcommon layout composition — enough to type a
// username/password. Real keyboard-layout support (via xkbcommon, matching
// how the rest of FocalDesk resolves keysyms) is a known follow-up once this
// needs to support non-US layouts.

pub const KEY_ESC: u32 = 1;
pub const KEY_BACKSPACE: u32 = 14;
pub const KEY_ENTER: u32 = 28;
pub const KEY_LEFTCTRL: u32 = 29;
pub const KEY_LEFTSHIFT: u32 = 42;
pub const KEY_RIGHTSHIFT: u32 = 54;
pub const KEY_LEFTALT: u32 = 56;
pub const KEY_CAPSLOCK: u32 = 58;
pub const KEY_RIGHTCTRL: u32 = 97;
pub const KEY_RIGHTALT: u32 = 100;

#[derive(Debug, Default, Clone, Copy)]
pub struct Modifiers {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    /// Toggled (not held) state of Caps Lock. Unlike the other fields, this
    /// doesn't track key-down/key-up: it flips once per press and ignores
    /// the matching release, same as every real keyboard driver treats it.
    pub caps: bool,
}

impl Modifiers {
    /// Updates modifier state for a modifier keycode. Returns `true` if
    /// `keycode` was a modifier key (and was therefore consumed here).
    pub fn track(&mut self, keycode: u32, pressed: bool) -> bool {
        match keycode {
            KEY_LEFTCTRL | KEY_RIGHTCTRL => self.ctrl = pressed,
            KEY_LEFTSHIFT | KEY_RIGHTSHIFT => self.shift = pressed,
            KEY_LEFTALT | KEY_RIGHTALT => self.alt = pressed,
            KEY_CAPSLOCK => {
                if pressed {
                    self.caps = !self.caps;
                }
            }
            _ => return false,
        }
        true
    }
}

/// Maps an evdev `KEY_F1..KEY_F12` code to a target VT number, for the
/// Ctrl+Alt+F<n> "drop to console" shortcut. Duplicated from the same table
/// in `focaldesk-engine`'s DRM backend (`crates/focaldesk-engine/src/backend/drm.rs`)
/// rather than shared, per the decision to keep the greeter standalone.
pub fn vt_switch_target(keycode: u32) -> Option<i32> {
    match keycode {
        59..=68 => Some((keycode - 59 + 1) as i32), // KEY_F1..KEY_F10 -> vt 1..10
        87 => Some(11),                             // KEY_F11 -> vt 11
        88 => Some(12),                             // KEY_F12 -> vt 12
        _ => None,
    }
}

/// US-QWERTY keycode -> char, for unshifted and shifted layers. `caps`
/// applies Caps Lock the way real keyboards do: it flips the case of
/// letters, but has no effect on digits/symbols (where only `shift` picks
/// the shifted glyph) — e.g. Caps Lock alone still types '1', not '!'.
pub fn keycode_to_char(keycode: u32, shift: bool, caps: bool) -> Option<char> {
    let (lower, upper) = ROWS
        .iter()
        .find(|(code, _, _)| *code == keycode)
        .map(|(_, lower, upper)| (*lower, *upper))?;
    let use_upper = if lower.is_ascii_alphabetic() {
        shift ^ caps
    } else {
        shift
    };
    Some(if use_upper { upper } else { lower })
}

// (evdev keycode, unshifted, shifted)
const ROWS: &[(u32, char, char)] = &[
    (2, '1', '!'),
    (3, '2', '@'),
    (4, '3', '#'),
    (5, '4', '$'),
    (6, '5', '%'),
    (7, '6', '^'),
    (8, '7', '&'),
    (9, '8', '*'),
    (10, '9', '('),
    (11, '0', ')'),
    (12, '-', '_'),
    (13, '=', '+'),
    (16, 'q', 'Q'),
    (17, 'w', 'W'),
    (18, 'e', 'E'),
    (19, 'r', 'R'),
    (20, 't', 'T'),
    (21, 'y', 'Y'),
    (22, 'u', 'U'),
    (23, 'i', 'I'),
    (24, 'o', 'O'),
    (25, 'p', 'P'),
    (26, '[', '{'),
    (27, ']', '}'),
    (30, 'a', 'A'),
    (31, 's', 'S'),
    (32, 'd', 'D'),
    (33, 'f', 'F'),
    (34, 'g', 'G'),
    (35, 'h', 'H'),
    (36, 'j', 'J'),
    (37, 'k', 'K'),
    (38, 'l', 'L'),
    (39, ';', ':'),
    (40, '\'', '"'),
    (43, '\\', '|'),
    (44, 'z', 'Z'),
    (45, 'x', 'X'),
    (46, 'c', 'C'),
    (47, 'v', 'V'),
    (48, 'b', 'B'),
    (49, 'n', 'N'),
    (50, 'm', 'M'),
    (51, ',', '<'),
    (52, '.', '>'),
    (53, '/', '?'),
    (57, ' ', ' '),
];
