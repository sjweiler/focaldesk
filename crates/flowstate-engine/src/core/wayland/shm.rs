
use smithay::delegate_shm;
use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::core::desktop::DesktopState;

impl ShmHandler for DesktopState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_shm!(DesktopState);

