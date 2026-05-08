use smithay::delegate_data_device;

use crate::core::desktop::DesktopState;

use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::selection::data_device::{
    DataDeviceHandler,
    DataDeviceState,
    WaylandDndGrabHandler,
};

impl SelectionHandler for DesktopState {
    type SelectionUserData = ();
}

impl WaylandDndGrabHandler for DesktopState {}

impl DataDeviceHandler for DesktopState {
    fn data_device_state(&mut self) -> &mut DataDeviceState {
        &mut self.data_device_state
    }
}

delegate_data_device!(DesktopState);
