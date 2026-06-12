//! KMS / backend “hardware” cursor plane vs in-framebuffer draw.
//!
//! Policy: try to upload the cursor to the hardware plane first each frame (or when the icon
//! changes). If that fails — wrong format, size over HW limits, atomic test-only failure, nested
//! compositor ignoring host cursor — set `CursorPlaneState::set_hardware_cursor_ready(false)`.
//! and draw the same pixmap in your renderer until upload succeeds again.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorPlaneState {
    /// `true` when the cursor is visible through the DRM stack (hardware plane or Smithay primary composition).
    hardware_cursor_ready: bool,
}

impl Default for CursorPlaneState {
    fn default() -> Self {
        Self {
            // Optimistic: try the DRM hardware cursor first; only draw into the framebuffer when we
            // learn the separate cursor element was skipped.
            hardware_cursor_ready: true,
        }
    }
}

impl CursorPlaneState {
    pub fn set_hardware_cursor_ready(&mut self, ready: bool) {
        self.hardware_cursor_ready = ready;
    }

    pub fn hardware_cursor_ready(&self) -> bool {
        self.hardware_cursor_ready
    }

    /// Composite the cursor in software when the pointer is visible and HW is not carrying it.
    pub fn software_cursor_needed(self, pointer_visible: bool) -> bool {
        pointer_visible && !self.hardware_cursor_ready
    }
}
