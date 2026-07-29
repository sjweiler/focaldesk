use anyhow::{Context, Result};
use focaldesk_settings_core::{Settings, load_settings};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::time::{MissedTickBehavior, interval};
use zbus::interface;

const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.focaldesk";
const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";

#[derive(Clone)]
struct LockdownBackend {
    location_disabled: Arc<AtomicBool>,
}

impl LockdownBackend {
    fn new() -> Self {
        Self {
            location_disabled: Arc::new(AtomicBool::new(location_is_disabled_from_disk())),
        }
    }

    fn refresh_location(&self) -> bool {
        let disabled = location_is_disabled_from_disk();
        self.location_disabled.swap(disabled, Ordering::AcqRel) != disabled
    }
}

#[interface(name = "org.freedesktop.impl.portal.Lockdown")]
impl LockdownBackend {
    #[zbus(property, name = "disable-printing")]
    fn disable_printing(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-save-to-disk")]
    fn disable_save_to_disk(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-application-handlers")]
    fn disable_application_handlers(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-location")]
    fn disable_location(&self) -> bool {
        self.location_disabled.load(Ordering::Acquire)
    }

    #[zbus(property, name = "disable-camera")]
    fn disable_camera(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-microphone")]
    fn disable_microphone(&self) -> bool {
        false
    }

    #[zbus(property, name = "disable-sound-output")]
    fn disable_sound_output(&self) -> bool {
        false
    }
}

fn location_is_disabled_from_disk() -> bool {
    location_is_disabled(&load_settings())
}

fn location_is_disabled(settings: &Settings) -> bool {
    !settings.privacy.location_services
}

pub async fn run() -> Result<()> {
    let backend = LockdownBackend::new();
    let connection = zbus::connection::Builder::session()
        .context("connect portal backend to the session bus")?
        .name(BUS_NAME)
        .context("validate Focaldesk portal bus name")?
        .serve_at(OBJECT_PATH, backend.clone())
        .context("export Focaldesk Lockdown portal")?
        .build()
        .await
        .context("start Focaldesk portal backend")?;

    let interface = connection
        .object_server()
        .interface::<_, LockdownBackend>(OBJECT_PATH)
        .await
        .context("look up exported Lockdown portal")?;

    let mut refresh = interval(Duration::from_millis(250));
    refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        refresh.tick().await;
        if backend.refresh_location() {
            interface
                .get()
                .await
                .disable_location_changed(interface.signal_emitter())
                .await
                .context("publish changed location-services setting")?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_name_and_path_match_portal_conventions() {
        assert_eq!(BUS_NAME, "org.freedesktop.impl.portal.desktop.focaldesk");
        assert_eq!(OBJECT_PATH, "/org/freedesktop/portal/desktop");
    }

    #[test]
    fn location_setting_maps_to_inverse_lockdown_property() {
        let mut settings = focaldesk_settings_core::default_settings();
        settings.privacy.location_services = false;
        assert!(location_is_disabled(&settings));

        settings.privacy.location_services = true;
        assert!(!location_is_disabled(&settings));
    }
}
