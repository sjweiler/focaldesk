use crate::notification::Notification;
use std::collections::VecDeque;

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

    pub fn push(&mut self, title: impl Into<String>, body: impl Into<String>) {
        let notif = Notification {
            id: self.next_id,
            title: title.into(),
            body: body.into(),
            timeout_ms: Some(5000),
        };

        self.next_id += 1;
        self.queue.push_back(notif);
    }

    pub fn pop(&mut self) -> Option<Notification> {
        self.queue.pop_front()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Notification> {
        self.queue.iter()
    }
}
