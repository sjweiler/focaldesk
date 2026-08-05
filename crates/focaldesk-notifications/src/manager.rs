use crate::notification::{
    DEFAULT_TIMEOUT, MAX_VISIBLE_NOTIFICATIONS, Notification, NotificationSnapshot,
};
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_HISTORY_NOTIFICATIONS: usize = 100;

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedNotification {
    id: u64,
    title: String,
    body: String,
    age_ms: u64,
    timeout_ms: Option<u64>,
    #[serde(default = "default_unread")]
    unread: bool,
}

fn default_unread() -> bool {
    true
}

pub struct NotificationManager {
    queue: VecDeque<Notification>,
    history: VecDeque<Notification>,
    next_id: u64,
    do_not_disturb: bool,
    history_limit: usize,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            history: VecDeque::new(),
            next_id: 1,
            do_not_disturb: false,
            history_limit: MAX_HISTORY_NOTIFICATIONS,
        }
    }

    pub fn set_do_not_disturb(&mut self, enabled: bool) {
        self.do_not_disturb = enabled;
    }

    pub fn do_not_disturb(&self) -> bool {
        self.do_not_disturb
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
            unread: true,
        };

        self.next_id += 1;
        self.queue.push_back(notif);
        id
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        if let Some(index) = self.queue.iter().position(|n| n.id == id) {
            return self.queue.remove(index).is_some();
        }
        if let Some(index) = self.history.iter().position(|n| n.id == id) {
            return self.history.remove(index).is_some();
        }
        false
    }

    pub fn expire(&mut self, now: Instant) -> bool {
        let before = self.queue.len();
        let mut expired = VecDeque::new();
        self.queue.retain(|notification| {
            if notification.is_expired(now) {
                expired.push_back(notification.clone());
                false
            } else {
                true
            }
        });
        self.history.extend(expired);
        while self.history.len() > self.history_limit {
            self.history.pop_front();
        }
        self.queue.len() != before
    }

    pub fn history_snapshots(&self, now: Instant) -> Vec<NotificationSnapshot> {
        self.history
            .iter()
            .chain(self.queue.iter())
            .rev()
            .map(|notification| notification.snapshot(now))
            .collect()
    }

    pub fn clear_history(&mut self) {
        self.queue.clear();
        self.history.clear();
    }

    pub fn mark_all_read(&mut self) {
        for notification in self.queue.iter_mut().chain(self.history.iter_mut()) {
            notification.unread = false;
        }
    }

    pub fn set_history_limit(&mut self, limit: usize) {
        self.history_limit = limit.clamp(25, MAX_HISTORY_NOTIFICATIONS);
        while self.history.len() > self.history_limit {
            self.history.pop_front();
        }
    }

    pub fn load_history(&mut self, path: &Path) {
        let Ok(bytes) = std::fs::read(path) else {
            return;
        };
        let Ok(entries) = serde_json::from_slice::<Vec<PersistedNotification>>(&bytes) else {
            return;
        };
        let now = Instant::now();
        for entry in entries
            .into_iter()
            .rev()
            .take(MAX_HISTORY_NOTIFICATIONS)
            .rev()
        {
            let age = Duration::from_millis(entry.age_ms);
            self.next_id = self.next_id.max(entry.id.saturating_add(1));
            self.history.push_back(Notification {
                id: entry.id,
                title: entry.title,
                body: entry.body,
                created_at: now.checked_sub(age).unwrap_or(now),
                timeout: entry.timeout_ms.map(Duration::from_millis),
                unread: entry.unread,
            });
        }
    }

    pub fn save_history(&self, path: &Path) -> std::io::Result<()> {
        let now = Instant::now();
        let entries: Vec<_> = self
            .history
            .iter()
            .map(|entry| PersistedNotification {
                id: entry.id,
                title: entry.title.clone(),
                body: entry.body.clone(),
                age_ms: now
                    .duration_since(entry.created_at)
                    .as_millis()
                    .min(u64::MAX as u128) as u64,
                timeout_ms: entry
                    .timeout
                    .map(|timeout| timeout.as_millis().min(u64::MAX as u128) as u64),
                unread: entry.unread,
            })
            .collect();
        let json = serde_json::to_vec_pretty(&entries).map_err(std::io::Error::other)?;
        let Some(parent) = path.parent() else {
            return Ok(());
        };
        std::fs::create_dir_all(parent)?;
        let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
        {
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&tmp)?;
            file.write_all(&json)?;
            file.sync_all()?;
            let mut perms = file.metadata()?.permissions();
            use std::os::unix::fs::PermissionsExt;
            perms.set_mode(0o600);
            std::fs::set_permissions(&tmp, perms)?;
        }
        std::fs::rename(tmp, path)
    }

    pub fn has_visible(&self, now: Instant) -> bool {
        self.queue
            .iter()
            .any(|notification| !notification.is_expired(now))
    }

    pub fn visible_snapshots(&self, now: Instant) -> Vec<NotificationSnapshot> {
        if self.do_not_disturb {
            return Vec::new();
        }
        self.queue
            .iter()
            .filter(|notification| !notification.is_expired(now))
            .rev()
            .take(MAX_VISIBLE_NOTIFICATIONS)
            .map(|notification| notification.snapshot(now))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn do_not_disturb_hides_notifications_without_dropping_them() {
        let mut manager = NotificationManager::new();
        manager.push_persistent("title", "body");
        manager.set_do_not_disturb(true);
        assert!(manager.visible_snapshots(Instant::now()).is_empty());
        manager.set_do_not_disturb(false);
        assert_eq!(manager.visible_snapshots(Instant::now()).len(), 1);
    }

    #[test]
    fn history_includes_active_and_expired_notifications() {
        let mut manager = NotificationManager::new();
        manager.push_persistent("active", "body");
        manager.push_with_timeout("expired", "body", Some(Duration::ZERO));
        manager.expire(Instant::now());
        let titles: Vec<_> = manager
            .history_snapshots(Instant::now())
            .into_iter()
            .map(|entry| entry.title)
            .collect();
        assert!(titles.contains(&"active".to_string()));
        assert!(titles.contains(&"expired".to_string()));
    }

    #[test]
    fn history_round_trips_through_private_state_file() {
        let path = std::env::temp_dir().join(format!(
            "focaldesk-notifications-{}.json",
            std::process::id()
        ));
        let mut manager = NotificationManager::new();
        manager.push_with_timeout("saved", "body", Some(Duration::ZERO));
        manager.expire(Instant::now());
        manager.save_history(&path).unwrap();
        let mut restored = NotificationManager::new();
        restored.load_history(&path);
        assert_eq!(restored.history_snapshots(Instant::now())[0].title, "saved");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clear_history_removes_active_and_expired_notifications() {
        let mut manager = NotificationManager::new();
        manager.push_persistent("active", "body");
        manager.push_with_timeout("expired", "body", Some(Duration::ZERO));
        manager.expire(Instant::now());
        manager.clear_history();
        assert!(manager.history_snapshots(Instant::now()).is_empty());
        assert!(!manager.has_visible(Instant::now()));
    }
}

impl Default for NotificationManager {
    fn default() -> Self {
        Self::new()
    }
}
