use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5_000);
pub const MAX_VISIBLE_NOTIFICATIONS: usize = 4;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationUrgency {
    Low,
    #[default]
    Normal,
    Critical,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotificationAction {
    pub key: String,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub created_at: Instant,
    pub timeout: Option<Duration>,
    pub unread: bool,
    pub app_name: String,
    pub desktop_entry: Option<String>,
    pub app_icon: Option<String>,
    pub category: Option<String>,
    pub urgency: NotificationUrgency,
    pub actions: Vec<NotificationAction>,
    pub transient: bool,
    pub resident: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotificationSnapshot {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub age: Duration,
    pub timeout: Option<Duration>,
    pub unread: bool,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub desktop_entry: Option<String>,
    #[serde(default)]
    pub app_icon: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub urgency: NotificationUrgency,
    #[serde(default)]
    pub actions: Vec<NotificationAction>,
    #[serde(default)]
    pub transient: bool,
    #[serde(default)]
    pub resident: bool,
}

impl Notification {
    pub fn is_expired(&self, now: Instant) -> bool {
        self.timeout
            .is_some_and(|timeout| now.duration_since(self.created_at) >= timeout)
    }

    pub fn snapshot(&self, now: Instant) -> NotificationSnapshot {
        NotificationSnapshot {
            id: self.id,
            title: self.title.clone(),
            body: self.body.clone(),
            age: now.duration_since(self.created_at),
            timeout: self.timeout,
            unread: self.unread,
            app_name: self.app_name.clone(),
            desktop_entry: self.desktop_entry.clone(),
            app_icon: self.app_icon.clone(),
            category: self.category.clone(),
            urgency: self.urgency,
            actions: self.actions.clone(),
            transient: self.transient,
            resident: self.resident,
        }
    }
}
