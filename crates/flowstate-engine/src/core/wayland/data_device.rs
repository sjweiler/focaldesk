#![allow(unused_imports)]

use smithay::{delegate_data_device, delegate_primary_selection};
use std::os::fd::OwnedFd;

use crate::core::desktop::DesktopState;

use smithay::input::dnd::DndGrabHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler, DataDeviceState, WaylandDndGrabHandler,
};
use smithay::wayland::selection::primary_selection::{
    PrimarySelectionHandler, PrimarySelectionState,
};
use smithay::wayland::selection::{SelectionHandler, SelectionSource, SelectionTarget};

impl SelectionHandler for DesktopState {
    type SelectionUserData = ();

    #[cfg(feature = "xwayland")]
    fn new_selection(
        &mut self,
        ty: SelectionTarget,
        source: Option<SelectionSource>,
        _seat: smithay::input::Seat<Self>,
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.new_selection(ty, source.map(|source| source.mime_types())) {
                flowstate_logging::flog(&format!("failed to set XWayland selection: {err}"));
            }
        }
    }

    #[cfg(feature = "xwayland")]
    fn send_selection(
        &mut self,
        ty: SelectionTarget,
        mime_type: String,
        fd: OwnedFd,
        _seat: smithay::input::Seat<Self>,
        _user_data: &Self::SelectionUserData,
    ) {
        if let Some(xwm) = self.xwm.as_mut() {
            if let Err(err) = xwm.send_selection(ty, mime_type, fd) {
                flowstate_logging::flog(&format!("failed to send XWayland selection: {err}"));
            }
        }
    }
}

impl WaylandDndGrabHandler for DesktopState {}
impl DndGrabHandler for DesktopState {}

impl DataDeviceHandler for DesktopState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

delegate_data_device!(DesktopState);

impl PrimarySelectionHandler for DesktopState {
    fn primary_selection_state(&mut self) -> &mut PrimarySelectionState {
        &mut self.primary_selection_state
    }
}

delegate_primary_selection!(DesktopState);
