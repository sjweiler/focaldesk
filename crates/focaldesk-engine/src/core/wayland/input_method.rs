//! Wayland text-input-v3/input-method-v2 integration.
//!
//! Text-input is a normal client protocol, while input-method can observe and
//! replace user input. The input-method global is therefore hidden from
//! unrecognized clients rather than advertised session-wide.

use smithay::desktop::{PopupKind, PopupManager};
use smithay::reexports::wayland_server::{protocol::wl_surface::WlSurface, Client};
use smithay::utils::{Logical, Rectangle};
use smithay::wayland::input_method::{InputMethodHandler, PopupSurface};
use smithay::wayland::seat::WaylandFocus;

use crate::core::desktop::DesktopState;

const DEFAULT_INPUT_METHOD_EXECUTABLES: &[&str] = &[
    "fcitx5",
    "fcitx5-wayland-launcher",
    "ibus-daemon",
    "maliit-keyboard",
];

fn executable_is_allowed(name: &str, configured: Option<&str>) -> bool {
    match configured {
        Some(list) => list
            .split(':')
            .map(str::trim)
            .filter(|candidate| !candidate.is_empty())
            .any(|candidate| candidate == name),
        None => DEFAULT_INPUT_METHOD_EXECUTABLES.contains(&name),
    }
}

pub(crate) fn input_method_client_allowed(client: &Client) -> bool {
    let Some(name) = super::client::client_executable_name(client) else {
        return false;
    };
    let configured = std::env::var("FOCALDESK_INPUT_METHOD_EXECUTABLES").ok();
    executable_is_allowed(&name, configured.as_deref())
}

impl InputMethodHandler for DesktopState {
    fn new_popup(&mut self, surface: PopupSurface) {
        if let Err(error) = self.popups.track_popup(PopupKind::from(surface)) {
            tracing::warn!(target: "focaldesk", %error, "failed to track input-method popup");
        }
    }

    fn dismiss_popup(&mut self, surface: PopupSurface) {
        if let Some(parent) = surface.get_parent().map(|parent| parent.surface.clone()) {
            let _ = PopupManager::dismiss_popup(&parent, &PopupKind::from(surface));
        }
    }

    fn popup_repositioned(&mut self, _surface: PopupSurface) {
        self.mark_focused_output_full_damage(crate::core::desktop::DamageSource::CommitBbox);
    }

    fn parent_geometry(&self, parent: &WlSurface) -> Rectangle<i32, Logical> {
        self.space
            .elements()
            .find_map(|window| {
                (window.wl_surface().as_deref() == Some(parent)).then(|| window.geometry())
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_input_methods_are_allowlisted() {
        assert!(executable_is_allowed("fcitx5", None));
        assert!(executable_is_allowed("ibus-daemon", None));
        assert!(!executable_is_allowed("untrusted-client", None));
    }

    #[test]
    fn configured_allowlist_replaces_defaults() {
        assert!(executable_is_allowed(
            "custom-ime",
            Some("custom-ime:other-ime")
        ));
        assert!(!executable_is_allowed(
            "fcitx5",
            Some("custom-ime:other-ime")
        ));
    }
}
