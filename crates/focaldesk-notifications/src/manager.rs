use crate::notification::{
    Notification, NotificationAction, NotificationSnapshot, NotificationUrgency, DEFAULT_TIMEOUT,
    MAX_VISIBLE_NOTIFICATIONS,
};
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

const MAX_HISTORY_NOTIFICATIONS: usize = 100;
const MAX_ACTIONS: usize = 16;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NotificationDetails {
    pub app_name: String,
    pub desktop_entry: Option<String>,
    pub app_icon: Option<String>,
    pub category: Option<String>,
    pub urgency: NotificationUrgency,
    pub actions: Vec<NotificationAction>,
    pub transient: bool,
    pub resident: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NotificationEvent {
    Closed { id: u32, reason: u32 },
    ActionInvoked { id: u32, action_key: String },
}

#[derive(serde::Serialize, serde::Deserialize)]
struct PersistedNotification {
    id: u64,
    title: String,
    body: String,
    age_ms: u64,
    timeout_ms: Option<u64>,
    #[serde(default = "default_unread")]
    unread: bool,
    #[serde(default)]
    app_name: String,
    #[serde(default)]
    desktop_entry: Option<String>,
    #[serde(default)]
    app_icon: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    urgency: NotificationUrgency,
}

fn default_unread() -> bool {
    true
}

fn sanitize_text(value: &str, limit: usize) -> String {
    let mut plain = String::with_capacity(value.len().min(limit));
    let mut in_tag = false;
    let mut written = 0usize;
    for character in value.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if in_tag => {}
            '\n' | '\t' => {
                plain.push(character);
                written += 1;
            }
            _ if !character.is_control() => {
                plain.push(character);
                written += 1;
            }
            _ => {}
        }
        if written >= limit {
            break;
        }
    }
    plain
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn sanitize_identifier(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}

pub struct NotificationManager {
    queue: VecDeque<Notification>,
    history: VecDeque<Notification>,
    next_id: u64,
    do_not_disturb: bool,
    history_limit: usize,
    events: VecDeque<NotificationEvent>,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            history: VecDeque::new(),
            next_id: 1,
            do_not_disturb: false,
            history_limit: MAX_HISTORY_NOTIFICATIONS,
            events: VecDeque::new(),
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
        self.push_standard(0, title, body, timeout, NotificationDetails::default())
    }

    pub fn push_standard(
        &mut self,
        replaces_id: u32,
        title: impl Into<String>,
        body: impl Into<String>,
        timeout: Option<Duration>,
        mut details: NotificationDetails,
    ) -> u64 {
        let replaced = (replaces_id != 0)
            .then(|| self.take_active_notification(u64::from(replaces_id)))
            .flatten();
        let id = replaced
            .as_ref()
            .map_or_else(|| self.allocate_id(), |notification| notification.id);
        details.actions.truncate(MAX_ACTIONS);
        let notif = Notification {
            id,
            title: sanitize_text(&title.into(), 256),
            body: sanitize_text(&body.into(), 4_096),
            created_at: Instant::now(),
            timeout,
            unread: true,
            app_name: sanitize_text(&details.app_name, 256),
            desktop_entry: details
                .desktop_entry
                .map(|value| sanitize_identifier(&value, 256)),
            app_icon: details.app_icon.map(|value| sanitize_text(&value, 1_024)),
            category: details
                .category
                .map(|value| sanitize_identifier(&value, 128)),
            urgency: details.urgency,
            actions: details
                .actions
                .into_iter()
                .filter_map(|action| {
                    let key = sanitize_identifier(&action.key, 128);
                    let label = sanitize_text(&action.label, 256);
                    (!key.is_empty() && !label.is_empty())
                        .then_some(NotificationAction { key, label })
                })
                .collect(),
            transient: details.transient,
            resident: details.resident,
        };

        self.queue.push_back(notif);
        id
    }

    fn allocate_id(&mut self) -> u64 {
        for _ in 0..u32::MAX {
            let candidate = self.next_id.clamp(1, u64::from(u32::MAX));
            self.next_id = if candidate == u64::from(u32::MAX) {
                1
            } else {
                candidate + 1
            };
            if !self
                .queue
                .iter()
                .chain(self.history.iter())
                .any(|notification| notification.id == candidate)
            {
                return candidate;
            }
        }
        1
    }

    fn take_active_notification(&mut self, id: u64) -> Option<Notification> {
        if let Some(index) = self
            .queue
            .iter()
            .position(|notification| notification.id == id)
        {
            return self.queue.remove(index);
        }
        None
    }

    fn take_historical_notification(&mut self, id: u64) -> Option<Notification> {
        let index = self
            .history
            .iter()
            .position(|notification| notification.id == id)?;
        self.history.remove(index)
    }

    pub fn dismiss(&mut self, id: u64) -> bool {
        self.close(id, 2)
    }

    pub fn close_requested(&mut self, id: u64) -> bool {
        self.close(id, 3)
    }

    fn close(&mut self, id: u64, reason: u32) -> bool {
        if self.take_active_notification(id).is_some() {
            self.events.push_back(NotificationEvent::Closed {
                id: id.min(u64::from(u32::MAX)) as u32,
                reason,
            });
            return true;
        }
        self.take_historical_notification(id).is_some()
    }

    pub fn invoke_action(&mut self, id: u64, action_key: &str) -> bool {
        let Some(notification) = self.queue.iter().find(|notification| notification.id == id)
        else {
            return false;
        };
        let Some(action) = notification
            .actions
            .iter()
            .find(|action| action.key == action_key)
        else {
            return false;
        };
        let action_key = action.key.clone();
        let resident = notification.resident;
        self.events.push_back(NotificationEvent::ActionInvoked {
            id: id.min(u64::from(u32::MAX)) as u32,
            action_key,
        });
        if !resident {
            let _ = self.close(id, 2);
        }
        true
    }

    pub fn drain_events(&mut self) -> Vec<NotificationEvent> {
        self.events.drain(..).collect()
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
        for mut notification in expired {
            self.events.push_back(NotificationEvent::Closed {
                id: notification.id.min(u64::from(u32::MAX)) as u32,
                reason: 1,
            });
            if !notification.transient {
                notification.actions.clear();
                self.history.push_back(notification);
            }
        }
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
        for notification in self.queue.drain(..) {
            self.events.push_back(NotificationEvent::Closed {
                id: notification.id.min(u64::from(u32::MAX)) as u32,
                reason: 2,
            });
        }
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
                app_name: entry.app_name,
                desktop_entry: entry.desktop_entry,
                app_icon: entry.app_icon,
                category: entry.category,
                urgency: entry.urgency,
                actions: Vec::new(),
                transient: false,
                resident: false,
            });
        }
    }

    pub fn save_history(&self, path: &Path) -> std::io::Result<()> {
        let now = Instant::now();
        let entries: Vec<_> = self
            .history
            .iter()
            .filter(|entry| !entry.transient)
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
                app_name: entry.app_name.clone(),
                desktop_entry: entry.desktop_entry.clone(),
                app_icon: entry.app_icon.clone(),
                category: entry.category.clone(),
                urgency: entry.urgency,
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
        self.queue.iter().any(|notification| {
            !notification.is_expired(now)
                && (!self.do_not_disturb || notification.urgency == NotificationUrgency::Critical)
        })
    }

    pub fn visible_snapshots(&self, now: Instant) -> Vec<NotificationSnapshot> {
        self.queue
            .iter()
            .filter(|notification| !notification.is_expired(now))
            .filter(|notification| {
                !self.do_not_disturb || notification.urgency == NotificationUrgency::Critical
            })
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

    #[test]
    fn standard_notification_replaces_an_active_id() {
        let mut manager = NotificationManager::new();
        let id = manager.push_standard(
            0,
            "Downloading",
            "10%",
            None,
            NotificationDetails::default(),
        );
        let replaced_id = manager.push_standard(
            id as u32,
            "Downloading",
            "90%",
            None,
            NotificationDetails::default(),
        );

        let visible = manager.visible_snapshots(Instant::now());
        assert_eq!(replaced_id, id);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].body, "90%");
    }

    #[test]
    fn critical_notifications_bypass_do_not_disturb() {
        let mut manager = NotificationManager::new();
        manager.push_persistent("normal", "hidden");
        manager.push_standard(
            0,
            "critical",
            "visible",
            None,
            NotificationDetails {
                urgency: NotificationUrgency::Critical,
                ..NotificationDetails::default()
            },
        );
        manager.set_do_not_disturb(true);

        let visible = manager.visible_snapshots(Instant::now());
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].title, "critical");
        assert!(manager.has_visible(Instant::now()));
    }

    #[test]
    fn invoking_non_resident_action_emits_action_then_close() {
        let mut manager = NotificationManager::new();
        let id = manager.push_standard(
            0,
            "Question",
            "Continue?",
            None,
            NotificationDetails {
                actions: vec![NotificationAction {
                    key: "continue".into(),
                    label: "Continue".into(),
                }],
                ..NotificationDetails::default()
            },
        );

        assert!(manager.invoke_action(id, "continue"));
        assert_eq!(
            manager.drain_events(),
            vec![
                NotificationEvent::ActionInvoked {
                    id: id as u32,
                    action_key: "continue".into(),
                },
                NotificationEvent::Closed {
                    id: id as u32,
                    reason: 2,
                },
            ]
        );
        assert!(manager.visible_snapshots(Instant::now()).is_empty());
    }

    #[test]
    fn expired_history_cannot_reinvoke_actions() {
        let mut manager = NotificationManager::new();
        let id = manager.push_standard(
            0,
            "Question",
            "Continue?",
            Some(Duration::ZERO),
            NotificationDetails {
                actions: vec![NotificationAction {
                    key: "continue".into(),
                    label: "Continue".into(),
                }],
                ..NotificationDetails::default()
            },
        );
        manager.expire(Instant::now());

        assert!(!manager.invoke_action(id, "continue"));
        assert!(manager.history_snapshots(Instant::now())[0]
            .actions
            .is_empty());
    }
}
