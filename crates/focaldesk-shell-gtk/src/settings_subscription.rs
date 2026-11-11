//! Key-filtered subscriptions to persisted application settings.
//!
//! Watch the directory so atomic replacements of settings.json also notify us.
//! Invalid intermediate writes are ignored; consumers retain their last snapshot.
use std::{cell::RefCell, rc::Rc};

use focaldesk_settings_core::{settings_path, Settings};
use gtk::{gio, glib, prelude::*};

pub struct SettingsSubscription(gio::FileMonitor);

impl Drop for SettingsSubscription {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

pub fn subscribe(
    keys: &[&str],
    changed: impl FnMut(Settings) + 'static,
) -> Result<SettingsSubscription, glib::Error> {
    subscribe_path(settings_path(), keys, changed)
}

fn subscribe_path(
    path: std::path::PathBuf,
    keys: &[&str],
    mut changed: impl FnMut(Settings) + 'static,
) -> Result<SettingsSubscription, glib::Error> {
    let directory = gio::File::for_path(path.parent().expect("settings directory"));
    let monitor =
        directory.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)?;
    let keys: Vec<String> = keys
        .iter()
        .map(|key| format!("/{}", key.replace('.', "/")))
        .collect();
    let previous = Rc::new(RefCell::new(None));
    let mut refresh = move || {
        let Ok(bytes) = std::fs::read(&path) else {
            return;
        };
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
            return;
        };
        let Ok(settings) = serde_json::from_value::<Settings>(value.clone()) else {
            return;
        };
        let selected: Vec<_> = keys.iter().map(|key| value.pointer(key).cloned()).collect();
        if previous.borrow().as_ref() != Some(&selected) {
            *previous.borrow_mut() = Some(selected);
            changed(settings);
        }
    };
    refresh();
    let refresh = RefCell::new(refresh);
    monitor.connect_changed(move |_, file, other, _| {
        if [Some(file), other]
            .into_iter()
            .flatten()
            .any(|file| file.basename().as_deref() == Some(std::path::Path::new("settings.json")))
        {
            (refresh.borrow_mut())();
        }
    });
    Ok(SettingsSubscription(monitor))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };

    #[test]
    fn subscription_filters_keys_and_survives_invalid_writes_and_replacement() {
        let context = glib::MainContext::new();
        let _guard = context.acquire().unwrap();
        context
            .with_thread_default(|| {
                let dir = std::env::temp_dir()
                    .join(format!("focaldesk-settings-watch-{}", std::process::id()));
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join("settings.json");
                let mut settings = focaldesk_settings_core::load_settings();
                settings.privacy.location_services = false;
                let write = |settings: &Settings| {
                    std::fs::write(&path, serde_json::to_vec(settings).unwrap()).unwrap()
                };
                write(&settings);
                let seen = Rc::new(RefCell::new(Vec::new()));
                let changes = seen.clone();
                let subscription = subscribe_path(
                    path.clone(),
                    &["privacy.location_services"],
                    move |settings| {
                        changes
                            .borrow_mut()
                            .push(settings.privacy.location_services);
                    },
                )
                .unwrap();
                let drain = || {
                    let deadline = Instant::now() + Duration::from_millis(1100);
                    while Instant::now() < deadline {
                        while context.pending() {
                            context.iteration(false);
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                };
                settings.accessibility.night_light.latitude = Some(42.0);
                write(&settings);
                drain();
                assert_eq!(*seen.borrow(), vec![false]);
                std::fs::write(&path, "{").unwrap();
                drain();
                assert_eq!(*seen.borrow(), vec![false]);
                settings.privacy.location_services = true;
                let replacement = dir.join("replacement");
                std::fs::write(&replacement, serde_json::to_vec(&settings).unwrap()).unwrap();
                std::fs::rename(replacement, &path).unwrap();
                drain();
                assert_eq!(*seen.borrow(), vec![false, true]);
                drop(subscription);
                settings.privacy.location_services = false;
                write(&settings);
                drain();
                assert_eq!(*seen.borrow(), vec![false, true]);
                std::fs::remove_dir_all(dir).unwrap();
            })
            .unwrap();
    }
}
