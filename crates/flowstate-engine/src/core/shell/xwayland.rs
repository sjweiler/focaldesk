#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XwaylandSurfaceRole {
    Toplevel,
    Dialog,
    Transient,
    Menu,
    Tooltip,
    Utility,
    Splash,
    Unknown,
}

impl Default for XwaylandSurfaceRole {
    fn default() -> Self {
        Self::Toplevel
    }
}

#[derive(Debug, Clone, Default)]
pub struct XwaylandWindowMeta {
    pub title: Option<String>,
    pub class: Option<String>,
    pub instance: Option<String>,

    pub override_redirect: bool,
    pub role: XwaylandSurfaceRole,
    pub transient_for: Option<u32>, // replace with your WindowId mapping later
}

impl XwaylandWindowMeta {
    pub fn new(
        title: Option<String>,
        class: Option<String>,
        instance: Option<String>,
    ) -> Self {
        Self {
            title,
            class,
            instance,
            override_redirect: false,
            role: XwaylandSurfaceRole::Toplevel,
            transient_for: None,
        }
    }

    pub fn with_override_redirect(mut self, value: bool) -> Self {
        self.override_redirect = value;
        self
    }

    pub fn with_role(mut self, role: XwaylandSurfaceRole) -> Self {
        self.role = role;
        self
    }

    pub fn with_transient_for(mut self, parent: Option<u32>) -> Self {
        self.transient_for = parent;
        self
    }

    pub fn should_be_managed(&self) -> bool {
        !self.override_redirect
    }

    pub fn should_float(&self) -> bool {
        self.override_redirect
            || matches!(
                self.role,
                XwaylandSurfaceRole::Dialog
                    | XwaylandSurfaceRole::Transient
                    | XwaylandSurfaceRole::Menu
                    | XwaylandSurfaceRole::Tooltip
                    | XwaylandSurfaceRole::Utility
                    | XwaylandSurfaceRole::Splash
            )
    }
}

