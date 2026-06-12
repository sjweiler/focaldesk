pub struct TopBar {
    pub title: String,
    pub meta: TopBarMeta,
    pub indicators: Vec<UiElement>,
    pub clock: ClockComponent,
    pub bounds: UiRect,
}

impl UiComponent fpr TopBar {
    pub fn layout(&mut self, screen: UiRect, scale: f32) {
        self.bounds = UiRect {
            x: 0,
            y: 0,
            w: screen.w,
            h: (48.0 * scale) as i32,
        };

        // Clock gets reserved space first.
        // Indicators get remaining space.
        // Title/meta shrink or hide last.
    }
}


