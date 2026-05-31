use crate::core::desktop::DesktopState;
use crate::core::render::RenderState;
use std::process::Command;

pub struct App {
    pub desktop: DesktopState,
    pub render: RenderState,
}

impl App {
    pub fn new(desktop: DesktopState, render: RenderState) -> Self {
        Self { desktop, render }
    }
}
