#[derive(Debug, Clone, Default)]
pub struct WaylandWindowMeta {
    pub title: Option<String>,
    pub app_id: Option<String>,
    pub is_dialog: bool,
    pub is_popup_like: bool,
}

impl WaylandWindowMeta {
    pub fn new(title: Option<String>, app_id: Option<String>) -> Self {
        Self {
            title,
            app_id,
            is_dialog: false,
            is_popup_like: false,
        }
    }

    pub fn with_dialog(mut self, value: bool) -> Self {
        self.is_dialog = value;
        self
    }

    pub fn with_popup_like(mut self, value: bool) -> Self {
        self.is_popup_like = value;
        self
    }
}
