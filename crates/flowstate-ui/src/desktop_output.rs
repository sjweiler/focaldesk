pub struct DesktopOutputConfig {
    pub show_topbar: bool,
    pub show_sidebar: bool,
    pub theme_id: ThemeId,
}

pub struct DesktopOutput {
    pub output_id: OutputId,

    pub logical_rect: Rectangle<i32, Logical>,
    pub scale_factor: f64,

    pub config: DesktopOutputConfig,

    pub topbar: Option<TopBar>,
    pub sidebar: Option<Sidebar>,
    pub workarea: WorkArea,
    pub overlays: OverlayManager,
}

impl DesktopOutput {
    pub fn layout(&mut self) { }

    pub fn render(&self, ...) { }

    pub fn hit_test(&self, ...) -> Option<UiHit> { }
}
