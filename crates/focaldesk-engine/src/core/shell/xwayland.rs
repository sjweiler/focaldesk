#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XwaylandSurfaceRole {
    #[default]
    Toplevel,
    Dialog,
    Transient,
    Menu,
    Tooltip,
    Utility,
    Splash,
    Unknown,
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
    #[cfg(feature = "xwayland")]
    pub fn from_surface(surface: &smithay::xwayland::X11Surface) -> Self {
        Self {
            title: non_empty(surface.title()),
            class: non_empty(surface.class()),
            instance: non_empty(surface.instance()),
            override_redirect: surface.is_override_redirect(),
            role: XwaylandSurfaceRole::from_surface(surface),
            transient_for: surface.is_transient_for(),
        }
    }

    pub fn new(title: Option<String>, class: Option<String>, instance: Option<String>) -> Self {
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

#[cfg(feature = "xwayland")]
fn non_empty(value: String) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

impl XwaylandSurfaceRole {
    #[cfg(feature = "xwayland")]
    pub fn from_surface(surface: &smithay::xwayland::X11Surface) -> Self {
        use smithay::xwayland::xwm::WmWindowType;

        if surface.is_transient_for().is_some() {
            return Self::Transient;
        }

        match surface.window_type() {
            Some(WmWindowType::Dialog) => Self::Dialog,
            Some(WmWindowType::DropdownMenu | WmWindowType::Menu | WmWindowType::PopupMenu) => {
                Self::Menu
            }
            Some(WmWindowType::Tooltip) => Self::Tooltip,
            Some(WmWindowType::Utility | WmWindowType::Toolbar) => Self::Utility,
            Some(WmWindowType::Splash) => Self::Splash,
            Some(WmWindowType::Normal) | None => Self::Toplevel,
            _ => Self::Unknown,
        }
    }
}
