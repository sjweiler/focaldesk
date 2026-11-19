//! Process-lifetime, transport-independent counters. Only the writer thread does I/O.
use super::DrmNode;
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::BTreeMap,
    path::PathBuf,
    rc::Rc,
    sync::mpsc::{sync_channel, SyncSender},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Default, Serialize)]
struct Display {
    state: &'static str,
    width: Option<u32>,
    height: Option<u32>,
    refresh_millihz: Option<u32>,
    presented_frames: u64,
}

#[derive(Default, Serialize)]
struct Device {
    state: &'static str,
    device_losses: u64,
    rebuild_attempts: u64,
    rebuild_successes: u64,
    // One incident until a replacement device is successfully created.
    #[serde(skip)]
    loss_observed: bool,
    #[serde(skip)]
    failed: bool,
    displays: BTreeMap<String, Display>,
}

#[derive(Clone)]
pub(super) struct DeviceMetrics(Rc<RefCell<Device>>);

impl DeviceMetrics {
    pub fn error(&self, error: &anyhow::Error) {
        let lost = error.chain().any(|cause| {
            cause.downcast_ref::<ash::vk::Result>() == Some(&ash::vk::Result::ERROR_DEVICE_LOST)
        });
        if lost {
            let mut device = self.0.borrow_mut();
            if !device.loss_observed {
                device.device_losses = device.device_losses.saturating_add(1);
                device.loss_observed = true;
            }
            device.failed = true;
            device.state = "failed";
        }
    }
    pub fn rebuild_started(&self) {
        let mut device = self.0.borrow_mut();
        device.rebuild_attempts = device.rebuild_attempts.saturating_add(1);
        device.state = "recovering";
    }
    pub fn rebuild_succeeded(&self) {
        let mut device = self.0.borrow_mut();
        device.rebuild_successes = device.rebuild_successes.saturating_add(1);
        device.loss_observed = false;
        device.failed = false;
        device.state = "ready";
    }
    pub fn failed(&self) {
        let mut device = self.0.borrow_mut();
        device.failed = true;
        device.state = "failed";
    }
    pub fn sample_gpu(&self, failed: bool, active: bool) {
        let mut device = self.0.borrow_mut();
        device.state = if failed || device.failed {
            "failed"
        } else if active {
            "ready"
        } else {
            "paused"
        };
    }
    pub fn active(&self) -> bool {
        self.0.borrow().state == "ready"
    }
    pub fn presented(&self, connector: &str) {
        let mut device = self.0.borrow_mut();
        let display = {
            if !device.displays.contains_key(connector) {
                device
                    .displays
                    .insert(connector.to_owned(), Display::default());
            }
            device
                .displays
                .get_mut(connector)
                .expect("registered display")
        };
        display.presented_frames = display.presented_frames.saturating_add(1);
    }
    pub fn display(
        &self,
        connector: &str,
        width: u32,
        height: u32,
        refresh: Option<u32>,
        active: bool,
    ) {
        let mut device = self.0.borrow_mut();
        let display = {
            if !device.displays.contains_key(connector) {
                device
                    .displays
                    .insert(connector.to_owned(), Display::default());
            }
            device
                .displays
                .get_mut(connector)
                .expect("registered display")
        };
        display.state = if active { "active" } else { "inactive" };
        display.width = Some(width);
        display.height = Some(height);
        display.refresh_millihz = refresh;
    }
}

pub(super) struct Monitor {
    devices: BTreeMap<String, DeviceMetrics>,
    started: Instant,
    epoch: String,
    next_sample: Instant,
    writer: Option<SyncSender<serde_json::Value>>,
    dropped_snapshots: u64,
}

impl Monitor {
    pub fn new() -> Self {
        let epoch = format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let writer = std::env::var_os("FOCALDESK_METRICS_PATH")
            .map(PathBuf::from)
            .and_then(|path| {
                if !path.is_absolute() {
                    focaldesk_logging::flog_warn!(
                        "FOCALDESK_METRICS_PATH must be absolute; snapshot export disabled"
                    );
                    return None;
                }
                let (sender, receiver) = sync_channel::<serde_json::Value>(1);
                match std::thread::Builder::new()
                    .name("metrics-writer".into())
                    .spawn(move || {
                        let mut warned = false;
                        for snapshot in receiver {
                            if let Err(error) = write_snapshot(&path, &snapshot) {
                                if !warned {
                                    focaldesk_logging::flog_warn!(
                                        "metrics snapshot write failed: {error}"
                                    );
                                    warned = true;
                                }
                            } else {
                                warned = false;
                            }
                        }
                    }) {
                    Ok(_) => Some(sender),
                    Err(error) => {
                        focaldesk_logging::flog_warn!("metrics writer unavailable: {error}");
                        None
                    }
                }
            });
        Self {
            devices: BTreeMap::new(),
            started: Instant::now(),
            epoch,
            next_sample: Instant::now(),
            writer,
            dropped_snapshots: 0,
        }
    }
    pub fn register_device(&mut self, node: DrmNode) -> DeviceMetrics {
        let identity = std::fs::canonicalize(format!(
            "/sys/dev/char/{}:{}/device",
            node.major(),
            node.minor()
        ))
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|_| format!("drm:{}:{}", node.major(), node.minor()));
        let metrics = self
            .devices
            .entry(identity)
            .or_insert_with(|| DeviceMetrics(Rc::new(RefCell::new(Device::default()))))
            .clone();
        {
            let mut device = metrics.0.borrow_mut();
            device.failed = false;
            device.loss_observed = false;
            device.state = "ready";
        }
        metrics
    }
    pub fn request_sample(&mut self) {
        self.next_sample = Instant::now();
    }
    pub fn due(&self) -> bool {
        self.writer.is_some() && Instant::now() >= self.next_sample
    }
    pub fn begin_sample(&mut self) {
        self.next_sample = Instant::now() + Duration::from_secs(1);
        for device in self.devices.values() {
            let mut device = device.0.borrow_mut();
            // Only live devices sampled below replace this absent marker.
            device.state = "absent";
            for display in device.displays.values_mut() {
                display.state = "notConfigured";
                display.width = None;
                display.height = None;
                display.refresh_millihz = None;
            }
        }
    }
    fn snapshot(&self) -> serde_json::Value {
        let devices: BTreeMap<_, _> = self
            .devices
            .iter()
            .map(|(identity, device)| {
                (
                    identity,
                    serde_json::to_value(&*device.0.borrow()).expect("metric values serialize"),
                )
            })
            .collect();
        serde_json::json!({
            "schema_version": 1, "producer_epoch": self.epoch,
            "uptime_seconds": self.started.elapsed().as_secs(),
            "sample_unix_ms": SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
            "dropped_snapshots": self.dropped_snapshots, "gpus": devices,
        })
    }
    pub fn publish(&mut self) {
        if let Some(writer) = &self.writer {
            if writer.try_send(self.snapshot()).is_err() {
                self.dropped_snapshots = self.dropped_snapshots.saturating_add(1);
            }
        }
    }
}

fn write_snapshot(path: &std::path::Path, snapshot: &serde_json::Value) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let temporary = path.with_extension(format!("metrics-{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)?;
    let result = (|| {
        serde_json::to_writer(&mut file, snapshot)?;
        file.write_all(b"\n")?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn losses_are_incidents_and_rebuilds_preserve_counts() {
        let metrics = DeviceMetrics(Rc::new(RefCell::new(Device::default())));
        let lost = anyhow::Error::new(ash::vk::Result::ERROR_DEVICE_LOST).context("submit");
        metrics.error(&lost);
        metrics.error(&lost);
        metrics.presented("DP-1");
        metrics.rebuild_started();
        metrics.rebuild_succeeded();
        metrics.rebuild_started(); // resume is a rebuild, not device loss
        metrics.rebuild_succeeded();
        metrics.error(&anyhow::Error::new(
            ash::vk::Result::ERROR_OUT_OF_DEVICE_MEMORY,
        ));
        let device = metrics.0.borrow();
        assert_eq!(
            (
                device.device_losses,
                device.rebuild_attempts,
                device.rebuild_successes
            ),
            (1, 2, 2)
        );
        assert_eq!(device.displays["DP-1"].presented_frames, 1);
        drop(device);
        metrics.error(&lost);
        assert_eq!(metrics.0.borrow().device_losses, 2);
    }
    #[test]
    fn topology_retirement_retains_counters_but_clears_mode() {
        let mut monitor = Monitor {
            devices: BTreeMap::new(),
            started: Instant::now(),
            epoch: "test".into(),
            next_sample: Instant::now(),
            writer: None,
            dropped_snapshots: 0,
        };
        let device = DeviceMetrics(Rc::new(RefCell::new(Device::default())));
        monitor
            .devices
            .insert("pci:0000:00:02.0".into(), device.clone());
        device.display("DP-1", 1920, 1080, Some(60000), true);
        device.presented("DP-1");
        monitor.begin_sample();
        let snapshot = monitor.snapshot();
        let gpu = &snapshot["gpus"]["pci:0000:00:02.0"];
        assert_eq!(gpu["state"], "absent");
        assert_eq!(gpu["displays"]["DP-1"]["presented_frames"], 1);
        assert!(gpu["displays"]["DP-1"]["width"].is_null());
        device.display("DP-1", 1280, 720, None, true);
        assert_eq!(device.0.borrow().displays["DP-1"].presented_frames, 1);
    }
}

#[cfg(test)]
mod snapshot_tests {
    use super::*;
    #[test]
    fn devices_are_independent_and_failed_state_survives_sampling() {
        let first = DeviceMetrics(Rc::new(RefCell::new(Device::default())));
        let second = DeviceMetrics(Rc::new(RefCell::new(Device::default())));
        first.error(&anyhow::Error::new(ash::vk::Result::ERROR_DEVICE_LOST));
        first.sample_gpu(false, true);
        second.sample_gpu(false, true);
        first.presented("DP-1");
        second.presented("DP-1");
        second.presented("DP-1");
        assert_eq!(first.0.borrow().state, "failed");
        assert_eq!(second.0.borrow().state, "ready");
        assert_eq!(second.0.borrow().device_losses, 0);
        assert_eq!(first.0.borrow().displays["DP-1"].presented_frames, 1);
        assert_eq!(second.0.borrow().displays["DP-1"].presented_frames, 2);
    }
    #[test]
    fn snapshot_replaces_atomically_with_private_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "focaldesk-metrics-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("snapshot.json");
        write_snapshot(&path, &serde_json::json!({"generation": 1})).unwrap();
        write_snapshot(&path, &serde_json::json!({"generation": 2})).unwrap();
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["generation"], 2);
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
