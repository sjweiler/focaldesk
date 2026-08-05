use gtk::prelude::*;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StateKind {
    Loading,
    Empty,
    Offline,
    PermissionDenied,
    ServiceUnavailable,
    Error,
    Success,
    Info,
}

impl StateKind {
    fn css_class(self) -> &'static str {
        match self {
            Self::Loading => "state-loading",
            Self::Empty => "state-empty",
            Self::Offline => "state-offline",
            Self::PermissionDenied => "state-permission",
            Self::ServiceUnavailable => "state-service",
            Self::Error => "state-error",
            Self::Success => "state-success",
            Self::Info => "state-info",
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Self::Loading => "content-loading-symbolic",
            Self::Empty => "edit-find-symbolic",
            Self::Offline => "network-offline-symbolic",
            Self::PermissionDenied => "changes-prevent-symbolic",
            Self::ServiceUnavailable => "system-run-symbolic",
            Self::Error => "dialog-error-symbolic",
            Self::Success => "emblem-ok-symbolic",
            Self::Info => "dialog-information-symbolic",
        }
    }
}

pub fn classify_status(message: &str) -> StateKind {
    let normalized = message.trim().to_ascii_lowercase();
    if normalized.starts_with("loading") || normalized.starts_with("querying") {
        StateKind::Loading
    } else if normalized.contains("permission")
        && (normalized.contains("denied") || normalized.contains("not allowed"))
    {
        StateKind::PermissionDenied
    } else if normalized.contains("service unavailable")
        || normalized.contains("daemon unavailable")
        || normalized.contains("scheduler is not running")
    {
        StateKind::ServiceUnavailable
    } else if normalized.contains("offline")
        || normalized.contains("disconnected")
        || normalized.ends_with("disabled")
    {
        StateKind::Offline
    } else if normalized.starts_with("no ") || normalized.contains("not found") {
        StateKind::Empty
    } else if normalized.contains("unable")
        || normalized.contains("failed")
        || normalized.contains("could not")
        || normalized.contains("error")
    {
        StateKind::Error
    } else if normalized.contains("connected")
        || normalized.contains("enabled")
        || normalized.contains("complete")
        || normalized.contains("created")
        || normalized.contains("renamed")
        || normalized.starts_with("moved ")
        || normalized.starts_with("copied ")
        || normalized.starts_with("pasted ")
        || normalized.starts_with("restored ")
    {
        StateKind::Success
    } else {
        StateKind::Info
    }
}

#[derive(Clone)]
pub struct StatusBanner {
    root: gtk::Box,
    icon: gtk::Image,
    spinner: gtk::Spinner,
    label: gtk::Label,
    details: gtk::Label,
    action: gtk::Button,
}

impl StatusBanner {
    pub fn new(message: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        root.add_css_class("fd-status-banner");
        root.set_accessible_role(gtk::AccessibleRole::Status);

        let icon = gtk::Image::new();
        icon.set_pixel_size(18);
        let spinner = gtk::Spinner::new();
        let text = gtk::Box::new(gtk::Orientation::Vertical, 2);
        text.set_hexpand(true);
        let label = gtk::Label::new(None);
        label.set_xalign(0.0);
        label.set_wrap(true);
        let details = gtk::Label::new(None);
        details.add_css_class("dim-label");
        details.set_xalign(0.0);
        details.set_wrap(true);
        details.set_visible(false);
        text.append(&label);
        text.append(&details);
        let action = gtk::Button::new();
        action.add_css_class("flat");
        action.set_visible(false);

        root.append(&icon);
        root.append(&spinner);
        root.append(&text);
        root.append(&action);

        let banner = Self {
            root,
            icon,
            spinner,
            label,
            details,
            action,
        };
        banner.set_text(message);
        banner
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn set_text(&self, message: &str) {
        self.set(classify_status(message), message);
    }

    pub fn set(&self, kind: StateKind, message: &str) {
        for class in STATE_CLASSES {
            self.root.remove_css_class(class);
        }
        self.root.add_css_class(kind.css_class());
        self.label.set_text(message);
        self.spinner.set_visible(kind == StateKind::Loading);
        if kind == StateKind::Loading {
            self.spinner.start();
            self.icon.set_visible(false);
        } else {
            self.spinner.stop();
            self.icon.set_icon_name(Some(kind.icon()));
            self.icon.set_visible(true);
        }
        self.root
            .update_property(&[gtk::accessible::Property::Label(message)]);
    }

    pub fn set_details(&self, details: Option<&str>) {
        self.details.set_text(details.unwrap_or_default());
        self.details
            .set_visible(details.is_some_and(|text| !text.is_empty()));
    }

    pub fn set_action_label(&self, label: Option<&str>) {
        self.action.set_label(label.unwrap_or_default());
        self.action.set_visible(label.is_some());
    }

    pub fn connect_action<F: Fn() + 'static>(&self, callback: F) {
        self.action.connect_clicked(move |_| callback());
    }
}

#[derive(Clone)]
pub struct StateView {
    root: gtk::Box,
    icon: gtk::Image,
    spinner: gtk::Spinner,
    title: gtk::Label,
    body: gtk::Label,
    details: gtk::Label,
    action: gtk::Button,
}

impl StateView {
    pub fn new(kind: StateKind, title: &str, body: &str) -> Self {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 8);
        root.add_css_class("fd-state-view");
        root.set_halign(gtk::Align::Center);
        root.set_valign(gtk::Align::Center);
        root.set_accessible_role(gtk::AccessibleRole::Status);
        let icon = gtk::Image::new();
        icon.set_pixel_size(48);
        let spinner = gtk::Spinner::new();
        spinner.set_size_request(40, 40);
        let title_label = gtk::Label::new(None);
        title_label.add_css_class("title-2");
        let body_label = gtk::Label::new(None);
        body_label.add_css_class("dim-label");
        body_label.set_wrap(true);
        body_label.set_justify(gtk::Justification::Center);
        let details = gtk::Label::new(None);
        details.add_css_class("dim-label");
        details.set_wrap(true);
        details.set_selectable(true);
        details.set_visible(false);
        let action = gtk::Button::new();
        action.add_css_class("suggested-action");
        action.set_halign(gtk::Align::Center);
        action.set_visible(false);
        root.append(&icon);
        root.append(&spinner);
        root.append(&title_label);
        root.append(&body_label);
        root.append(&details);
        root.append(&action);

        let view = Self {
            root,
            icon,
            spinner,
            title: title_label,
            body: body_label,
            details,
            action,
        };
        view.set(kind, title, body);
        view
    }

    pub fn widget(&self) -> gtk::Widget {
        self.root.clone().upcast()
    }

    pub fn set(&self, kind: StateKind, title: &str, body: &str) {
        for class in STATE_CLASSES {
            self.root.remove_css_class(class);
        }
        self.root.add_css_class(kind.css_class());
        self.title.set_text(title);
        self.body.set_text(body);
        self.spinner.set_visible(kind == StateKind::Loading);
        if kind == StateKind::Loading {
            self.spinner.start();
            self.icon.set_visible(false);
        } else {
            self.spinner.stop();
            self.icon.set_icon_name(Some(kind.icon()));
            self.icon.set_visible(true);
        }
        self.root.update_property(&[
            gtk::accessible::Property::Label(title),
            gtk::accessible::Property::Description(body),
        ]);
    }

    pub fn set_details(&self, details: Option<&str>) {
        self.details.set_text(details.unwrap_or_default());
        self.details
            .set_visible(details.is_some_and(|text| !text.is_empty()));
    }

    pub fn set_action_label(&self, label: Option<&str>) {
        self.action.set_label(label.unwrap_or_default());
        self.action.set_visible(label.is_some());
    }

    pub fn connect_action<F: Fn() + 'static>(&self, callback: F) {
        self.action.connect_clicked(move |_| callback());
    }
}

#[derive(Clone)]
pub struct ToastOverlay {
    overlay: gtk::Overlay,
    revealer: gtk::Revealer,
    banner: StatusBanner,
}

impl ToastOverlay {
    pub fn new(content: &impl IsA<gtk::Widget>) -> Self {
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(content));
        let revealer = gtk::Revealer::new();
        revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
        revealer.set_halign(gtk::Align::Center);
        revealer.set_valign(gtk::Align::End);
        revealer.set_margin_bottom(18);
        let banner = StatusBanner::new("");
        banner.root.add_css_class("fd-toast");
        revealer.set_child(Some(&banner.widget()));
        overlay.add_overlay(&revealer);
        Self {
            overlay,
            revealer,
            banner,
        }
    }

    pub fn widget(&self) -> gtk::Widget {
        self.overlay.clone().upcast()
    }

    pub fn show(&self, kind: StateKind, message: &str) {
        self.banner.set(kind, message);
        self.revealer.set_reveal_child(true);
        let revealer = self.revealer.clone();
        gtk::glib::timeout_add_local_once(Duration::from_secs(4), move || {
            revealer.set_reveal_child(false);
        });
    }

    pub fn set_action_label(&self, label: Option<&str>) {
        self.banner.set_action_label(label);
    }

    pub fn connect_action<F: Fn() + 'static>(&self, callback: F) {
        self.banner.connect_action(callback);
    }
}

const STATE_CLASSES: [&str; 8] = [
    "state-loading",
    "state-empty",
    "state-offline",
    "state-permission",
    "state-service",
    "state-error",
    "state-success",
    "state-info",
];

#[cfg(test)]
mod tests {
    use super::{classify_status, StateKind};

    #[test]
    fn classifies_common_user_facing_statuses() {
        assert_eq!(classify_status("Loading Wi-Fi state"), StateKind::Loading);
        assert_eq!(classify_status("No devices found"), StateKind::Empty);
        assert_eq!(classify_status("Wi-Fi is disabled"), StateKind::Offline);
        assert_eq!(
            classify_status("Permission denied"),
            StateKind::PermissionDenied
        );
        assert_eq!(
            classify_status("AI daemon unavailable"),
            StateKind::ServiceUnavailable
        );
        assert_eq!(classify_status("Unable to connect"), StateKind::Error);
        assert_eq!(classify_status("Connected to Moonbase"), StateKind::Success);
    }
}
