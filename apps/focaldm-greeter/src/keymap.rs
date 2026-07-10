// Minimal keyboard handling for the greeter, keyed on the xkb-offset codes
// smithay's libinput backend hands us. This is a fixed US-QWERTY table, not
// full xkbcommon layout composition — enough to type a username/password.
// Real keyboard-layout support (via xkbcommon, matching how the rest of
// FocalDesk resolves keysyms) is a known follow-up once this needs to
// support non-US layouts.
//
// All codes below are evdev keycodes (per linux/input-event-codes.h) `+8`:
// smithay's `KeyboardKeyEvent::key_code()` (libinput backend) returns the
// raw evdev code plus the xkbcommon keycode offset, not the raw evdev code
// itself — see `crates/focaldesk-engine/src/backend/drm.rs`'s
// `vt_switch_target` for the same convention on the real compositor's side.

pub const KEY_ESC: u32 = 1 + 8;
pub const KEY_BACKSPACE: u32 = 14 + 8;
pub const KEY_ENTER: u32 = 28 + 8;
pub const KEY_LEFTCTRL: u32 = 29 + 8;
pub const KEY_LEFTSHIFT: u32 = 42 + 8;
pub const KEY_RIGHTSHIFT: u32 = 54 + 8;
pub const KEY_LEFTALT: u32 = 56 + 8;
pub const KEY_CAPSLOCK: u32 = 58 + 8;
pub const KEY_RIGHTCTRL: u32 = 97 + 8;
pub const KEY_RIGHTALT: u32 = 100 + 8;

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

/// Maps an xkb-offset `KEY_F1..KEY_F12` code to a target VT number, for the
/// Ctrl+Alt+F<n> "drop to console" shortcut. Duplicated from the same table
/// in `focaldesk-engine`'s DRM backend (`crates/focaldesk-engine/src/backend/drm.rs`)
/// rather than shared, per the decision to keep the greeter standalone.
pub fn vt_switch_target(keycode: u32) -> Option<i32> {
    match keycode {
        67..=76 => Some((keycode - 67 + 1) as i32), // KEY_F1..KEY_F10 -> vt 1..10
        95 => Some(11),                             // KEY_F11 -> vt 11
        96 => Some(12),                             // KEY_F12 -> vt 12
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

// (xkb-offset keycode = evdev keycode + 8, unshifted, shifted)
const ROWS: &[(u32, char, char)] = &[
    (10, '1', '!'),
    (11, '2', '@'),
    (12, '3', '#'),
    (13, '4', '$'),
    (14, '5', '%'),
    (15, '6', '^'),
    (16, '7', '&'),
    (17, '8', '*'),
    (18, '9', '('),
    (19, '0', ')'),
    (20, '-', '_'),
    (21, '=', '+'),
    (24, 'q', 'Q'),
    (25, 'w', 'W'),
    (26, 'e', 'E'),
    (27, 'r', 'R'),
    (28, 't', 'T'),
    (29, 'y', 'Y'),
    (30, 'u', 'U'),
    (31, 'i', 'I'),
    (32, 'o', 'O'),
    (33, 'p', 'P'),
    (34, '[', '{'),
    (35, ']', '}'),
    (38, 'a', 'A'),
    (39, 's', 'S'),
    (40, 'd', 'D'),
    (41, 'f', 'F'),
    (42, 'g', 'G'),
    (43, 'h', 'H'),
    (44, 'j', 'J'),
    (45, 'k', 'K'),
    (46, 'l', 'L'),
    (47, ';', ':'),
    (48, '\'', '"'),
    (51, '\\', '|'),
    (52, 'z', 'Z'),
    (53, 'x', 'X'),
    (54, 'c', 'C'),
    (55, 'v', 'V'),
    (56, 'b', 'B'),
    (57, 'n', 'N'),
    (58, 'm', 'M'),
    (59, ',', '<'),
    (60, '.', '>'),
    (61, '/', '?'),
    (65, ' ', ' '),
];
