//! Keyboard focus types for [`crate::core::wayland::seat::DesktopState`] so xdg popup grabs
//! (`PopupManager::grab_popup`) can be installed (requires [`KeyboardFocus`]: [`From<PopupKind>`](PopupKind)).

use smithay::backend::input::KeyState;
use smithay::desktop::{PopupKind, Window};
use smithay::input::keyboard::{KeyboardTarget, KeysymHandle, ModifiersState};
use smithay::input::Seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::utils::{IsAlive, Serial};
use smithay::wayland::seat::WaylandFocus;
use std::borrow::Cow;
use std::fmt;
use tracing::warn;

use crate::core::desktop::DesktopState;

fn x11_surface(window: &Window) -> Option<&smithay::xwayland::X11Surface> {
    window.x11_surface()
}

#[derive(Clone, PartialEq)]
pub enum KeyboardFocusTarget {
    Window(Window),
    Popup(PopupKind),
}

impl fmt::Debug for KeyboardFocusTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyboardFocusTarget::Window(w) => f.debug_tuple("Window").field(w).finish(),
            KeyboardFocusTarget::Popup(p) => f.debug_tuple("Popup").field(p).finish(),
        }
    }
}

impl IsAlive for KeyboardFocusTarget {
    fn alive(&self) -> bool {
        match self {
            KeyboardFocusTarget::Window(w) => w.alive(),
            KeyboardFocusTarget::Popup(p) => p.alive(),
        }
    }
}

impl KeyboardTarget<DesktopState> for KeyboardFocusTarget {
    fn enter(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        keys: Vec<KeysymHandle<'_>>,
        serial: Serial,
    ) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::enter(x11, seat, data, keys, serial);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::enter(cow.as_ref(), seat, data, keys, serial);
                    return;
                }
                warn!(?w, "keyboard focus enter skipped: window has no wl_surface");
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::enter(p.wl_surface(), seat, data, keys, serial)
            }
        }
    }

    fn leave(&self, seat: &Seat<DesktopState>, data: &mut DesktopState, serial: Serial) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::leave(x11, seat, data, serial);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::leave(cow.as_ref(), seat, data, serial);
                    return;
                }
                warn!(?w, "keyboard focus leave skipped: window has no wl_surface");
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::leave(p.wl_surface(), seat, data, serial)
            }
        }
    }

    fn key(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        key: KeysymHandle<'_>,
        state: KeyState,
        serial: Serial,
        time: u32,
    ) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::key(x11, seat, data, key, state, serial, time);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::key(cow.as_ref(), seat, data, key, state, serial, time);
                    return;
                }
                warn!(?w, "keyboard focus key skipped: window has no wl_surface");
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::key(p.wl_surface(), seat, data, key, state, serial, time)
            }
        }
    }

    fn modifiers(
        &self,
        seat: &Seat<DesktopState>,
        data: &mut DesktopState,
        modifiers: ModifiersState,
        serial: Serial,
    ) {
        match self {
            KeyboardFocusTarget::Window(w) => {
                if let Some(x11) = x11_surface(w) {
                    KeyboardTarget::modifiers(x11, seat, data, modifiers, serial);
                    return;
                }
                if let Some(cow) = w.wl_surface() {
                    KeyboardTarget::modifiers(cow.as_ref(), seat, data, modifiers, serial);
                    return;
                }
                warn!(
                    ?w,
                    "keyboard focus modifiers skipped: window has no wl_surface"
                );
            }
            KeyboardFocusTarget::Popup(p) => {
                KeyboardTarget::modifiers(p.wl_surface(), seat, data, modifiers, serial)
            }
        }
    }
}

impl WaylandFocus for KeyboardFocusTarget {
    fn wl_surface(&self) -> Option<Cow<'_, WlSurface>> {
        match self {
            KeyboardFocusTarget::Window(w) => w.wl_surface(),
            KeyboardFocusTarget::Popup(p) => Some(Cow::Borrowed(p.wl_surface())),
        }
    }
}

impl From<PopupKind> for KeyboardFocusTarget {
    fn from(p: PopupKind) -> Self {
        Self::Popup(p)
    }
}

impl From<Window> for KeyboardFocusTarget {
    fn from(w: Window) -> Self {
        Self::Window(w)
    }
}

impl From<KeyboardFocusTarget> for WlSurface {
    fn from(t: KeyboardFocusTarget) -> Self {
        match t {
            KeyboardFocusTarget::Window(w) => w
                .wl_surface()
                .expect("keyboard focus: wayland window")
                .into_owned(),
            KeyboardFocusTarget::Popup(p) => p.into(),
        }
    }
}
