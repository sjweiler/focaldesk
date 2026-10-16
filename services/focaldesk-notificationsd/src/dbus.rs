use anyhow::{Context, Result};
use focaldesk_notifications::{
    NotificationAction, NotificationDetails, NotificationEvent, NotificationManager,
    NotificationUrgency,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::time::{interval, MissedTickBehavior};
use zbus::interface;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::OwnedValue;

const BUS_NAME: &str = "org.freedesktop.Notifications";
const OBJECT_PATH: &str = "/org/freedesktop/Notifications";

#[derive(Clone)]
struct FreedesktopNotifications {
    manager: Arc<Mutex<NotificationManager>>,
}

fn string_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    String::try_from(hints.get(key)?.clone()).ok()
}

fn bool_hint(hints: &HashMap<String, OwnedValue>, key: &str) -> bool {
    hints
        .get(key)
        .cloned()
        .and_then(|value| bool::try_from(value).ok())
        .unwrap_or(false)
}

fn urgency_hint(hints: &HashMap<String, OwnedValue>) -> NotificationUrgency {
    match hints
        .get("urgency")
        .cloned()
        .and_then(|value| u8::try_from(value).ok())
    {
        Some(0) => NotificationUrgency::Low,
        Some(2) => NotificationUrgency::Critical,
        _ => NotificationUrgency::Normal,
    }
}

fn notification_actions(values: Vec<String>) -> Vec<NotificationAction> {
    values
        .chunks_exact(2)
        .take(16)
        .map(|pair| NotificationAction {
            key: pair[0].clone(),
            label: pair[1].clone(),
        })
        .collect()
}

fn notification_timeout(expire_timeout: i32, urgency: NotificationUrgency) -> Option<Duration> {
    match expire_timeout {
        0 => None,
        value if value > 0 => Some(Duration::from_millis(value as u64)),
        _ if urgency == NotificationUrgency::Critical => None,
        _ => Some(focaldesk_notifications::notification::DEFAULT_TIMEOUT),
    }
}

#[interface(name = "org.freedesktop.Notifications")]
impl FreedesktopNotifications {
    #[zbus(name = "GetCapabilities")]
    fn get_capabilities(&self) -> Vec<String> {
        vec!["actions".into(), "body".into()]
    }

    #[zbus(name = "GetServerInformation")]
    fn get_server_information(&self) -> (String, String, String, String) {
        (
            "FocalDesk Notifications".into(),
            "FocalDesk".into(),
            env!("CARGO_PKG_VERSION").into(),
            "1.2".into(),
        )
    }

    #[allow(clippy::too_many_arguments)]
    #[zbus(name = "Notify")]
    fn notify(
        &self,
        app_name: String,
        replaces_id: u32,
        app_icon: String,
        summary: String,
        body: String,
        actions: Vec<String>,
        hints: HashMap<String, OwnedValue>,
        expire_timeout: i32,
    ) -> u32 {
        let urgency = urgency_hint(&hints);
        let details = NotificationDetails {
            app_name,
            desktop_entry: string_hint(&hints, "desktop-entry"),
            app_icon: (!app_icon.is_empty())
                .then_some(app_icon)
                .or_else(|| string_hint(&hints, "image-path")),
            category: string_hint(&hints, "category"),
            urgency,
            actions: notification_actions(actions),
            transient: bool_hint(&hints, "transient"),
            resident: bool_hint(&hints, "resident"),
        };
        let timeout = notification_timeout(expire_timeout, urgency);
        self.manager
            .lock()
            .unwrap()
            .push_standard(replaces_id, summary, body, timeout, details) as u32
    }

    #[zbus(name = "CloseNotification")]
    fn close_notification(&self, id: u32) {
        let _ = self.manager.lock().unwrap().close_requested(u64::from(id));
    }

    #[zbus(signal, name = "NotificationClosed")]
    async fn notification_closed(
        emitter: &SignalEmitter<'_>,
        id: u32,
        reason: u32,
    ) -> zbus::Result<()>;

    #[zbus(signal, name = "ActionInvoked")]
    async fn action_invoked(
        emitter: &SignalEmitter<'_>,
        id: u32,
        action_key: String,
    ) -> zbus::Result<()>;
}

pub async fn run(manager: Arc<Mutex<NotificationManager>>, state_path: PathBuf) -> Result<()> {
    let service = FreedesktopNotifications {
        manager: manager.clone(),
    };
    let connection = zbus::connection::Builder::session()
        .context("connect notification service to the session bus")?
        .name(BUS_NAME)
        .context("validate notification service bus name")?
        .serve_at(OBJECT_PATH, service)
        .context("export freedesktop notification service")?
        .build()
        .await
        .context("start freedesktop notification service")?;

    let interface = connection
        .object_server()
        .interface::<_, FreedesktopNotifications>(OBJECT_PATH)
        .await
        .context("look up exported notification interface")?;
    let mut tick = interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        tick.tick().await;
        let events = {
            let mut manager = manager.lock().unwrap();
            let expired = manager.expire(Instant::now());
            let events = manager.drain_events();
            if expired || !events.is_empty() {
                let _ = manager.save_history(&state_path);
            }
            events
        };
        for event in events {
            match event {
                NotificationEvent::Closed { id, reason } => {
                    FreedesktopNotifications::notification_closed(
                        interface.signal_emitter(),
                        id,
                        reason,
                    )
                    .await?;
                }
                NotificationEvent::ActionInvoked { id, action_key } => {
                    FreedesktopNotifications::action_invoked(
                        interface.signal_emitter(),
                        id,
                        action_key,
                    )
                    .await?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_are_bounded_pairs() {
        let actions = notification_actions(vec!["default".into(), "Open".into(), "orphan".into()]);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].key, "default");
    }

    #[test]
    fn timeout_follows_the_desktop_notification_contract() {
        assert_eq!(
            notification_timeout(1_500, NotificationUrgency::Normal),
            Some(Duration::from_millis(1_500))
        );
        assert_eq!(notification_timeout(0, NotificationUrgency::Normal), None);
        assert_eq!(
            notification_timeout(-1, NotificationUrgency::Critical),
            None
        );
    }
}
