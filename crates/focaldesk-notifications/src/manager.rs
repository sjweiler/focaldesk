use crate::notification::{
    DEFAULT_TIMEOUT, MAX_VISIBLE_NOTIFICATIONS, Notification, NotificationSnapshot,
};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

pub struct NotificationManager {
    queue: VecDeque<Notification>,
    next_id: u64,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            next_id: 1,
        }
    }

    pub fn push(&mut self, title: impl Into<String>, body: impl Into<String>) -> u64 {
        self.push_with_timeout(title, body, Some(DEFAULT_TIMEOUT))
    }

    pub fn push_persistent(&mut self, title: impl Into<String>, body: impl Into<String>) -> u64 {
        self.push_with_timeout(title, body, None)
    }

    pub fn push_with_timeout(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        timeout: Option<Duration>,
    ) -> u64 {
        let id = self.next_id;
        let notif = Notification {
            id,
            title: title.into(),
            body: body.into(),
            created_at: Instant::now(),
            timeout,
        };

        self.next_id += 1;
        self.queue.push_back(notif);
        id
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        let Some(index) = self
            .queue
            .iter()
            .position(|notification| notification.id == id)
        else {
            return false;
        };

        self.queue.remove(index).is_some()
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.queue.len();
        self.queue
            .retain(|notification| !notification.is_expired(now));
        self.queue.len() != before
    }

    pub fn has_visible(&self, now: Instant) -> bool {
        self.queue
            .iter()
            .any(|notification| !notification.is_expired(now))
    }

    pub fn visible_snapshots(&self, now: Instant) -> Vec<NotificationSnapshot> {
        self.queue
            .iter()
            .filter(|notification| !notification.is_expired(now))
            .rev()
            .take(MAX_VISIBLE_NOTIFICATIONS)
            .map(|notification| notification.snapshot(now))
            .collect()
    }
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}
