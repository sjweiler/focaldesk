use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_millis(5_000);
pub const MAX_VISIBLE_NOTIFICATIONS: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub created_at: Instant,
    pub timeout: Option<Duration>,
    pub unread: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct NotificationSnapshot {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub age: Duration,
    pub timeout: Option<Duration>,
    pub unread: bool,
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
        }
    }
}
