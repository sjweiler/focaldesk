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

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum DisplayTopology {
    Connected,
    Disabled,
    Disconnected,
    Unknown,
}

#[derive(Default, Serialize)]
struct Display {
    state: &'static str,
    width: Option<u32>,
    height: Option<u32>,
    refresh_millihz: Option<u32>,
    presented_frames: u64,
    failures: u64,
    recoveries: u64,
    #[serde(skip)]
    failed: bool,
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
    #[serde(skip)]
    topology: Option<BTreeMap<String, DisplayTopology>>,
}

#[derive(Clone)]
pub(super) struct DeviceMetrics(Rc<RefCell<Device>>);

fn connector_name(name: &str) -> &str {
    // Secondary wl_output names may append a GPU suffix to avoid duplicates.
    name.split_once('@')
        .map_or(name, |(connector, _)| connector)
}

fn topology_state(
    topology: Option<&BTreeMap<String, DisplayTopology>>,
    connector: &str,
) -> DisplayTopology {
    match topology {
        Some(topology) => topology
            .get(connector_name(connector))
            .copied()
            .unwrap_or(DisplayTopology::Disconnected),
        None => DisplayTopology::Unknown,
    }
}

fn fallback_state(topology: DisplayTopology, failed: bool) -> &'static str {
    match topology {
        DisplayTopology::Disabled => "disabled",
        DisplayTopology::Disconnected => "disconnected",
        _ if failed => "failed",
        _ => "notConfigured",
    }
}

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
        let Device {
            displays, topology, ..
        } = &mut *device;
        for (name, display) in displays {
            display.state = fallback_state(topology_state(topology.as_ref(), name), display.failed);
        }
    }
    pub fn active(&self) -> bool {
        self.0.borrow().state == "ready"
    }
    pub fn update_display_topology(&self, topology: BTreeMap<String, DisplayTopology>) {
        let mut device = self.0.borrow_mut();
        for (connector, display) in &mut device.displays {
            if matches!(
                topology_state(Some(&topology), connector),
                DisplayTopology::Disabled | DisplayTopology::Disconnected
            ) {
                // A removed/disabled output does not complete a recovery.
                display.failed = false;
            }
        }
        for (connector, state) in &topology {
            if *state == DisplayTopology::Disabled
                && !device
                    .displays
                    .keys()
                    .any(|name| connector_name(name) == connector)
            {
                device
                    .displays
                    .insert(connector.clone(), Display::default());
            }
        }
        device.topology = Some(topology);
    }
    pub fn display_failed(&self, connector: &str) {
        let mut device = self.0.borrow_mut();
        if matches!(
            topology_state(device.topology.as_ref(), connector),
            DisplayTopology::Disabled | DisplayTopology::Disconnected
        ) {
            return;
        }
        let display = device.displays.entry(connector.to_owned()).or_default();
        if !display.failed {
            display.failed = true;
            display.failures = display.failures.saturating_add(1);
        }
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
        if std::mem::take(&mut display.failed) {
            display.recoveries = display.recoveries.saturating_add(1);
        }
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
        let topology = topology_state(device.topology.as_ref(), connector);
        if matches!(
            topology,
            DisplayTopology::Disabled | DisplayTopology::Disconnected
        ) {
            return;
        }
        let display = device.displays.entry(connector.to_owned()).or_default();
        display.state = if display.failed {
            "failed"
        } else if active {
            "active"
        } else {
            "inactive"
        };
        display.width = Some(width);
        display.height = Some(height);
        display.refresh_millihz = refresh;
    }
}

// Upper edges in microseconds. The last bin counts samples above the final edge.
const INPUT_DELAY_EDGES_US: [u64; 10] = [
    250, 500, 1000, 2000, 4000, 8000, 16000, 32000, 64000, 128000,
];

#[derive(Clone, Copy)]
pub(super) enum InputKind {
    Keyboard,
    PointerMotion,
    PointerButton,
}

#[derive(Default, Serialize)]
struct InputLatency {
    samples: u64,
    total_us: u64,
    max_us: Option<u64>,
    bins: [u64; INPUT_DELAY_EDGES_US.len() + 1],
}

impl InputLatency {
    fn observe(&mut self, event_us: u64, callback_us: u64) {
        if event_us == 0 {
            return;
        }
        let Some(delay_us) = callback_us.checked_sub(event_us) else {
            return;
        };
        let bin = INPUT_DELAY_EDGES_US.partition_point(|&edge| edge < delay_us);
        self.samples = self.samples.saturating_add(1);
        self.total_us = self.total_us.saturating_add(delay_us);
        self.max_us = Some(self.max_us.unwrap_or_default().max(delay_us));
        self.bins[bin] = self.bins[bin].saturating_add(1);
    }
}

#[derive(Default, Serialize)]
struct InputDispatch {
    keyboard: InputLatency,
    pointer_motion: InputLatency,
    pointer_button: InputLatency,
}

fn monotonic_microseconds() -> Option<u64> {
    let mut timestamp = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: timestamp is valid writable storage for clock_gettime.
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) } != 0 {
        return None;
    }
    Some((timestamp.tv_sec as u64) * 1_000_000 + (timestamp.tv_nsec as u64) / 1000)
}

pub(super) struct Monitor {
    devices: BTreeMap<String, DeviceMetrics>,
    started: Instant,
    epoch: String,
    next_sample: Instant,
    writer: Option<SyncSender<serde_json::Value>>,
    dropped_snapshots: u64,
    input_dispatch: InputDispatch,
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
            input_dispatch: InputDispatch::default(),
        }
    }
    pub fn input_dispatched(&mut self, kind: InputKind, event_us: u64) {
        if self.writer.is_none() {
            return;
        }
        let Some(callback_us) = monotonic_microseconds() else {
            return;
        };
        let series = match kind {
            InputKind::Keyboard => &mut self.input_dispatch.keyboard,
            InputKind::PointerMotion => &mut self.input_dispatch.pointer_motion,
            InputKind::PointerButton => &mut self.input_dispatch.pointer_button,
        };
        series.observe(event_us, callback_us);
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
            for display in device.displays.values_mut() {
                display.failed = false;
            }
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
                display.state = "disconnected";
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
            "input_dispatch_delay": {
                "bin_upper_edges_us": INPUT_DELAY_EDGES_US,
                "series": self.input_dispatch,
            },
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
            input_dispatch: InputDispatch::default(),
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

#[cfg(test)]
#[test]
fn input_delay_bins_measure_elapsed_time_and_skip_invalid_timestamps() {
    let mut latency = InputLatency::default();
    assert_eq!(latency.max_us, None);
    latency.observe(0, 1_000_000);
    latency.observe(1_000_001, 1_000_000);
    latency.observe(1_000_000, 1_000_000);
    latency.observe(999_750, 1_000_000);
    latency.observe(999_749, 1_000_000);
    latency.observe(1_000_000, 1_200_000);
    assert_eq!(latency.samples, 4);
    assert_eq!(latency.total_us, 200_501);
    assert_eq!(latency.max_us, Some(200_000));
    assert_eq!(latency.bins[0], 2);
    assert_eq!(latency.bins[1], 1);
    assert_eq!(latency.bins[INPUT_DELAY_EDGES_US.len()], 1);
}

#[cfg(test)]
#[test]
fn display_failures_distinguish_recovery_from_unplug_and_disable() {
    let metrics = DeviceMetrics(Rc::new(RefCell::new(Device::default())));
    let set_topology = |state| {
        metrics.update_display_topology(BTreeMap::from([("DP-3".to_owned(), state)]));
    };
    let sample = |expected_state, failures, recoveries| {
        metrics.sample_gpu(false, true);
        metrics.display("DP-3", 3840, 2160, Some(120_000), true);
        let device = metrics.0.borrow();
        let display = &device.displays["DP-3"];
        assert_eq!(display.state, expected_state);
        assert_eq!(
            (display.failures, display.recoveries),
            (failures, recoveries)
        );
    };
    set_topology(DisplayTopology::Connected);
    sample("active", 0, 0);
    metrics.display_failed("DP-3");
    metrics.display_failed("DP-3");
    sample("failed", 1, 0);
    metrics.presented("DP-3");
    sample("active", 1, 1);
    set_topology(DisplayTopology::Disconnected);
    metrics.display_failed("DP-3");
    sample("disconnected", 1, 1);
    set_topology(DisplayTopology::Disabled);
    metrics.display_failed("DP-3");
    sample("disabled", 1, 1);
    set_topology(DisplayTopology::Connected);
    metrics.display_failed("DP-3");
    set_topology(DisplayTopology::Disconnected);
    set_topology(DisplayTopology::Connected);
    metrics.presented("DP-3");
    sample("active", 2, 1);
}
