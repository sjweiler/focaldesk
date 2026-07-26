//! Toolkit-independent accessibility semantics for compositor-owned UI.
//!
//! These types deliberately do not depend on AT-SPI. The compositor can keep a
//! small semantic tree in its render model while a future out-of-process bridge
//! translates snapshots and events to the platform accessibility API.

/// The interaction contract exposed by a compositor-owned UI element.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessibleRole {
    Button,
    ToggleButton,
    Slider,
    Menu,
    MenuItem,
    Dialog,
    Status,
    Label,
    List,
    ListItem,
    Tab,
}

impl AccessibleRole {
    pub const fn is_interactive(self) -> bool {
        matches!(
            self,
            Self::Button
                | Self::ToggleButton
                | Self::Slider
                | Self::MenuItem
                | Self::ListItem
                | Self::Tab
        )
    }
}

/// Semantic metadata that cannot be reliably inferred from drawing state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessibleInfo {
    pub role: AccessibleRole,
    pub name: String,
    pub description: Option<String>,
    pub value_text: Option<String>,
    pub key_shortcut: Option<String>,
    pub checked: Option<bool>,
    pub expanded: Option<bool>,
    /// Whether changes to this element should be announced without moving focus.
    pub live: bool,
}

impl AccessibleInfo {
    pub fn new(role: AccessibleRole, name: impl Into<String>) -> Self {
        Self {
            role,
            name: name.into(),
            description: None,
            value_text: None,
            key_shortcut: None,
            checked: None,
            expanded: None,
            live: false,
        }
    }

    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn value_text(mut self, value: impl Into<String>) -> Self {
        self.value_text = Some(value.into());
        self
    }

    pub fn key_shortcut(mut self, shortcut: impl Into<String>) -> Self {
        self.key_shortcut = Some(shortcut.into());
        self
    }

    pub fn checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }

    pub fn expanded(mut self, expanded: bool) -> Self {
        self.expanded = Some(expanded);
        self
    }

    pub fn live(mut self, live: bool) -> Self {
        self.live = live;
        self
    }
}
