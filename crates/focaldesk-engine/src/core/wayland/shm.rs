use smithay::wayland::shm::{ShmHandler, ShmState};

use crate::core::desktop::DesktopState;

impl ShmHandler for DesktopState {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}
