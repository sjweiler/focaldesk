use crate::assets::{CursorAssets, CursorImage};
use crate::cursor::{CursorIcon as FlowCursorIcon, CursorState};
use crate::hardware::CursorPlaneState;
use anyhow::Result;
use smithay::input::pointer::CursorIcon;

/// Compositor-facing cursor: theme bitmaps plus whether the KMS / backend cursor plane is in use.
pub struct CursorManager {
    state: CursorState,
    position: (f64, f64),
    visible: bool,
    assets: CursorAssets,
    /// Nominal cursor size in logical pixels (before `scale`).
    base_size: u32,
    scale: f32,
    plane: CursorPlaneState,
}

impl CursorManager {
    pub fn new(base_size: u32, scale: f32) -> Self {
        Self {
            state: CursorState::new(),
            position: (0.0, 0.0),
            visible: true,
            assets: CursorAssets::new(),
            base_size,
            scale,
            plane: CursorPlaneState::default(),
        }
    }

    pub fn set_base_size_and_scale(&mut self, base_size: u32, scale: f32) {
        self.base_size = base_size;
        self.scale = scale;
    }

    pub fn set_icon(&mut self, icon: CursorIcon) {
        self.state.set(icon.into());
    }

    pub fn set_flow_icon(&mut self, icon: FlowCursorIcon) {
        self.state.set(icon);
    }

    pub fn current_flow_icon(&self) -> FlowCursorIcon {
        self.state.current()
    }

    pub fn move_to(&mut self, x: f64, y: f64) {
        self.position = (x, y);
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn plane_state(&self) -> CursorPlaneState {
        self.plane
    }

    /// Call after attempting to program the hardware cursor (KMS cursor plane, nested winit icon, …).
    pub fn set_hardware_cursor_ready(&mut self, ready: bool) {
        self.plane.set_hardware_cursor_ready(ready);
    }

    /// When true, composite the cursor in your GL/scene pass (hardware path failed or is unavailable).
    pub fn software_cursor_needed(&self) -> bool {
        self.plane.software_cursor_needed(self.visible)
    }

    pub fn current_image(&mut self) -> Result<&CursorImage> {
        self.assets
            .image_for(self.state.current(), self.base_size, self.scale)
    }

    pub fn position(&self) -> (f64, f64) {
        self.position
    }

    pub fn visible(&self) -> bool {
        self.visible
    }
}
