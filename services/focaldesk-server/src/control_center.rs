use anyhow::{Context, Result, bail};
use chrono::{Local, TimeZone, Utc};
use focaldesk_ai::AiService;
use focaldesk_ipc::transport::{self, PeerPolicy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

pub const CONTROL_CENTER_SOCKET_NAME: &str = "control-center.sock";
pub const CONTROL_CENTER_SOCKET_ENV: &str = "FOCALDESK_CONTROL_CENTER_SOCKET";

const CONTROL_CENTER_POLICY: PeerPolicy<'static> = PeerPolicy {
    endpoint: "control-center",
    allowed_executables: &[],
    allowed_units: &["focaldesk-control-center.service"],
};

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
pub enum ControlCenterRequest {
    GetSnapshot,
    Subscribe {
        #[serde(default = "default_interval_ms")]
        interval_ms: u64,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind")]
pub enum ControlCenterResponse {
    Snapshot { snapshot: DashboardSnapshot },
    Error { message: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardSnapshot {
    pub system: SystemInfo,
    pub health: HealthInfo,
    pub metrics: Metrics,
    pub session: SessionInfo,
    pub displays: Vec<DisplayInfo>,
    pub gpu: GpuInfo,
    pub services: Vec<ServiceInfo>,
    pub logs: Vec<LogEntry>,
    pub ai: AiInfo,
    pub credentials: CredentialHealth,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub hostname: String,
    pub os: String,
    pub kernel: String,
    pub uptime: String,
    pub last_updated: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthInfo {
    pub status: String,
    pub operational: usize,
    pub total: usize,
    pub latency: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metrics {
    pub cpu: u8,
    pub cpu_cores: usize,
    pub memory: u8,
    pub memory_used_gb: f32,
    pub memory_total_gb: f32,
    pub gpu: u8,
    pub vram: u8,
    pub temperature: u16,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub user: String,
    pub compositor: String,
    pub desktop: String,
    pub state: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayInfo {
    pub id: String,
    pub name: String,
    pub resolution: String,
    pub hz: u16,
    pub primary: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GpuInfo {
    pub name: String,
    pub renderer: String,
    pub driver: String,
    pub api: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub status: String,
    pub uptime: String,
    pub cpu: f32,
    pub memory: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEntry {
    pub id: u64,
    pub time: String,
    pub level: String,
    pub source: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiInfo {
    pub status: String,
    pub active_requests: u32,
    pub default_provider: String,
    pub provider_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialHealth {
    pub status: String,
    pub socket_available: bool,
}

#[derive(Debug, Clone, Copy)]
struct CpuCounters {
    total: u64,
    idle: u64,
}

struct TelemetryCollector {
    cpu: Mutex<Option<CpuCounters>>,
    logs: Mutex<Option<(Instant, Vec<LogEntry>)>>,
}

impl TelemetryCollector {
    fn new() -> Self {
        Self {
            cpu: Mutex::new(read_cpu_counters()),
            logs: Mutex::new(None),
        }
    }

    fn cpu_percent(&self) -> u8 {
        let current = match read_cpu_counters() {
            Some(value) => value,
            None => return 0,
        };
        let Ok(mut previous) = self.cpu.lock() else {
            return 0;
        };
        let percent = previous.map_or(0, |old| {
            let total = current.total.saturating_sub(old.total);
            let idle = current.idle.saturating_sub(old.idle);
            if total == 0 {
                0
            } else {
                (((total.saturating_sub(idle)) * 100) / total).min(100) as u8
            }
        });
        *previous = Some(current);
        percent
    }

    async fn logs(&self) -> Vec<LogEntry> {
        if let Ok(cache) = self.logs.lock() {
            if let Some((collected_at, entries)) = cache.as_ref() {
                if collected_at.elapsed() < Duration::from_secs(15) {
                    return entries.clone();
                }
            }
        }
        let entries = tokio::task::spawn_blocking(read_journal_logs)
            .await
            .unwrap_or_default();
        if let Ok(mut cache) = self.logs.lock() {
            *cache = Some((Instant::now(), entries.clone()));
        }
        entries
    }
}

pub fn control_center_socket_path() -> Result<PathBuf> {
    transport::socket_path(CONTROL_CENTER_SOCKET_ENV, CONTROL_CENTER_SOCKET_NAME)
        .map_err(anyhow::Error::msg)
}

pub async fn serve_control_center_ipc(ai_service: Arc<AiService>) -> Result<()> {
    let path = control_center_socket_path()?;
    let listener = transport::bind_user_socket(&path).with_context(|| {
        format!(
            "failed to bind Control Center IPC socket {}",
            path.display()
        )
    })?;
    listener
        .set_nonblocking(true)
        .context("configure Control Center IPC listener")?;
    let listener = UnixListener::from_std(listener).context("adopt Control Center IPC listener")?;
    let collector = Arc::new(TelemetryCollector::new());

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("Control Center IPC accept failed")?;
        let authorized =
            if std::env::var_os("FOCALDESK_CONTROL_CENTER_ALLOW_DEVELOPMENT_NODE").is_some() {
                transport::require_same_user(&stream)
            } else {
                transport::require_authorized_peer(&stream, CONTROL_CENTER_POLICY).map(|_| ())
            };
        if let Err(err) = authorized {
            tracing::warn!(target: "focaldesk.control_center", error = %err, "rejected Control Center IPC peer");
            continue;
        }

        let service = ai_service.clone();
        let collector = collector.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_connection(service, collector, stream).await {
                tracing::warn!(target: "focaldesk.control_center", error = %err, "Control Center IPC connection failed");
            }
        });
    }
}

async fn handle_connection(
    ai_service: Arc<AiService>,
    collector: Arc<TelemetryCollector>,
    mut stream: UnixStream,
) -> Result<()> {
    let mut input = Vec::new();
    (&mut stream)
        .take(transport::MAX_REQUEST_BYTES + 1)
        .read_to_end(&mut input)
        .await
        .context("read Control Center IPC request")?;
    if input.len() as u64 > transport::MAX_REQUEST_BYTES {
        bail!(
            "Control Center IPC request exceeds {} bytes",
            transport::MAX_REQUEST_BYTES
        );
    }

    let request = match transport::decode_message::<ControlCenterRequest>(&input) {
        Ok(request) => request,
        Err(message) => {
            write_response(&mut stream, &ControlCenterResponse::Error { message }).await?;
            return Ok(());
        }
    };

    match request {
        ControlCenterRequest::GetSnapshot => {
            let snapshot = collect_snapshot(&ai_service, &collector).await;
            write_response(&mut stream, &ControlCenterResponse::Snapshot { snapshot }).await?;
        }
        ControlCenterRequest::Subscribe { interval_ms } => {
            let interval_ms = clamp_interval(interval_ms);
            loop {
                let snapshot = collect_snapshot(&ai_service, &collector).await;
                if write_response(&mut stream, &ControlCenterResponse::Snapshot { snapshot })
                    .await
                    .is_err()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(interval_ms)).await;
            }
        }
    }
    Ok(())
}

async fn write_response(stream: &mut UnixStream, response: &ControlCenterResponse) -> Result<()> {
    let mut output = transport::encode_message(response).map_err(anyhow::Error::msg)?;
    output.push(b'\n');
    stream
        .write_all(&output)
        .await
        .context("write Control Center IPC response")
}

async fn collect_snapshot(
    ai_service: &AiService,
    collector: &TelemetryCollector,
) -> DashboardSnapshot {
    let ai_status = ai_service.status();
    let runtime = runtime_directory();
    let uptime_seconds = read_uptime_seconds();
    let services = collect_services(&runtime, uptime_seconds, &ai_status.default_provider);
    let operational = services
        .iter()
        .filter(|service| service.status == "running")
        .count();
    let displays = collect_displays();
    let (gpu, gpu_load, vram, temperature) = collect_gpu();
    let logs = collector.logs().await;
    let credential_available = runtime.join("secrets.sock").exists();

    let (memory, memory_used_gb, memory_total_gb) = memory_metrics();
    DashboardSnapshot {
        system: SystemInfo {
            hostname: read_trimmed("/etc/hostname").unwrap_or_else(|| "focaldesk".into()),
            os: read_os_name(),
            kernel: format!(
                "Linux {}",
                read_trimmed("/proc/sys/kernel/osrelease").unwrap_or_else(|| "unknown".into())
            ),
            uptime: format_uptime(uptime_seconds),
            last_updated: Utc::now().to_rfc3339(),
        },
        health: HealthInfo {
            status: if operational == services.len() {
                "healthy"
            } else {
                "degraded"
            }
            .into(),
            operational,
            total: services.len(),
            latency: 0,
        },
        metrics: Metrics {
            cpu: collector.cpu_percent(),
            cpu_cores: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            memory,
            memory_used_gb,
            memory_total_gb,
            gpu: gpu_load,
            vram,
            temperature,
        },
        session: SessionInfo {
            user: std::env::var("USER").unwrap_or_else(|_| "unknown".into()),
            compositor: if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                "Wayland".into()
            } else {
                "Unknown".into()
            },
            desktop: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "FocalDesk".into()),
            state: if runtime.join("desktop.sock").exists() {
                "Active"
            } else {
                "Unavailable"
            }
            .into(),
            id: std::env::var("XDG_SESSION_ID").unwrap_or_else(|_| "local-session".into()),
        },
        displays,
        gpu,
        services,
        logs,
        ai: AiInfo {
            status: "running".into(),
            active_requests: ai_status.active_requests,
            default_provider: ai_status.default_provider,
            provider_count: ai_status.provider_count,
        },
        credentials: CredentialHealth {
            status: if credential_available {
                "healthy"
            } else {
                "unavailable"
            }
            .into(),
            socket_available: credential_available,
        },
    }
}

fn default_interval_ms() -> u64 {
    3_000
}

fn clamp_interval(value: u64) -> u64 {
    value.clamp(1_000, 30_000)
}

fn runtime_directory() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/run/user/{}", unsafe { libc::geteuid() })))
        .join("focaldesk")
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn read_os_name() -> String {
    let contents = fs::read_to_string("/etc/os-release").unwrap_or_default();
    contents
        .lines()
        .find_map(|line| {
            line.strip_prefix("PRETTY_NAME=")
                .map(|value| value.trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "Linux".into())
}

fn read_uptime_seconds() -> u64 {
    read_trimmed("/proc/uptime")
        .and_then(|value| value.split_whitespace().next()?.parse::<f64>().ok())
        .map(|value| value as u64)
        .unwrap_or_default()
}

fn format_uptime(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    format!("{days}d {hours}h {minutes}m")
}

fn read_cpu_counters() -> Option<CpuCounters> {
    let line = fs::read_to_string("/proc/stat")
        .ok()?
        .lines()
        .next()?
        .to_string();
    let values = line
        .split_whitespace()
        .skip(1)
        .filter_map(|value| value.parse::<u64>().ok())
        .collect::<Vec<_>>();
    if values.len() < 4 {
        return None;
    }
    let idle = values[3] + values.get(4).copied().unwrap_or_default();
    Some(CpuCounters {
        total: values.iter().sum(),
        idle,
    })
}

fn memory_metrics() -> (u8, f32, f32) {
    let values: HashMap<String, u64> = fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let (key, rest) = line.split_once(':')?;
            Some((
                key.to_string(),
                rest.split_whitespace().next()?.parse().ok()?,
            ))
        })
        .collect();
    let total = values.get("MemTotal").copied().unwrap_or_default();
    let available = values.get("MemAvailable").copied().unwrap_or_default();
    if total == 0 {
        return (0, 0.0, 0.0);
    }
    let used = total.saturating_sub(available);
    let percent = (((used * 100) / total).min(100)) as u8;
    const KIB_PER_GIB: f32 = 1024.0 * 1024.0;
    (
        percent,
        ((used as f32 / KIB_PER_GIB) * 10.0).round() / 10.0,
        ((total as f32 / KIB_PER_GIB) * 10.0).round() / 10.0,
    )
}

fn collect_services(runtime: &Path, _uptime_seconds: u64, ai_provider: &str) -> Vec<ServiceInfo> {
    let definitions = [
        ("shell", "Focal Shell", "Desktop compositor", "desktop.sock"),
        (
            "server",
            "FocalDesk Server",
            "AI and diagnostics IPC",
            "focaldesk-ai.sock",
        ),
        (
            "settings",
            "Settings Service",
            "Typed configuration service",
            "settings.sock",
        ),
        (
            "controls",
            "Control Service",
            "Audio, network, and Bluetooth controls",
            "controls.sock",
        ),
        (
            "credentials",
            "Credential Service",
            "Secrets and keyring broker",
            "secrets.sock",
        ),
        (
            "notifications",
            "Notification Daemon",
            "Desktop notification bus",
            "notifications.sock",
        ),
        (
            "automation",
            "Automation Runtime",
            "Policy-constrained automation",
            "focaldesk-automation.sock",
        ),
    ];
    definitions
        .into_iter()
        .map(|(id, name, description, socket)| {
            let socket_path = runtime.join(socket);
            let running = socket_path.exists();
            let description = if id == "server" && !ai_provider.is_empty() {
                format!("AI and diagnostics IPC · {ai_provider}")
            } else {
                description.to_string()
            };
            ServiceInfo {
                id: id.into(),
                name: name.into(),
                description,
                status: if running { "running" } else { "stopped" }.into(),
                uptime: if running {
                    socket_age(&socket_path).unwrap_or_else(|| "Available".into())
                } else {
                    "—".into()
                },
                cpu: 0.0,
                memory: "—".into(),
            }
        })
        .collect()
}

fn socket_age(path: &Path) -> Option<String> {
    let created = fs::metadata(path).ok()?.modified().ok()?;
    let elapsed = SystemTime::now().duration_since(created).ok()?.as_secs();
    Some(format_uptime(elapsed))
}

fn collect_displays() -> Vec<DisplayInfo> {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut displays = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if read_trimmed(path.join("status")).as_deref() != Some("connected") {
                return None;
            }
            let kernel_name = entry.file_name().to_string_lossy().to_string();
            let connector = kernel_name
                .split_once('-')
                .map(|(_, value)| value)
                .unwrap_or(&kernel_name)
                .to_string();
            let mode = read_trimmed(path.join("modes"))
                .and_then(|modes| modes.lines().next().map(str::to_string))
                .unwrap_or_else(|| "unknown".into());
            Some(DisplayInfo {
                id: connector.clone(),
                name: connector,
                resolution: mode.replace('x', " × "),
                hz: 60,
                primary: false,
            })
        })
        .collect::<Vec<_>>();
    displays.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(first) = displays.first_mut() {
        first.primary = true;
    }
    displays
}

fn collect_gpu() -> (GpuInfo, u8, u8, u16) {
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return (unknown_gpu(), 0, 0, 0);
    };
    let mut candidates = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("card") || name.contains('-') {
                return None;
            }
            let device = entry.path().join("device");
            let uevent = fs::read_to_string(device.join("uevent")).ok()?;
            let values = parse_key_values(&uevent, '=');
            let driver = values
                .get("DRIVER")
                .cloned()
                .unwrap_or_else(|| "unknown".into());
            let pci = values
                .get("PCI_ID")
                .cloned()
                .unwrap_or_else(|| "unknown".into());
            let class = values.get("PCI_CLASS").cloned().unwrap_or_default();
            Some((class == "30000", device, driver, pci))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|candidate| !candidate.0);
    let Some((_, device, driver, pci)) = candidates.first() else {
        return (unknown_gpu(), 0, 0, 0);
    };
    let load = read_trimmed(device.join("gpu_busy_percent"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    let used = read_trimmed(device.join("mem_info_vram_used"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let total = read_trimmed(device.join("mem_info_vram_total"))
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0);
    let vram = if total == 0 {
        0
    } else {
        ((used * 100) / total).min(100) as u8
    };
    let temperature = read_gpu_temperature(device);
    let vendor = match pci.split(':').next() {
        Some("10DE") => "NVIDIA",
        Some("1002") => "AMD",
        Some("8086") => "Intel",
        _ => "PCI",
    };
    (
        GpuInfo {
            name: format!("{vendor} GPU {pci}"),
            renderer: driver.clone(),
            driver: driver.clone(),
            api: "DRM/KMS".into(),
        },
        load,
        vram,
        temperature,
    )
}

fn unknown_gpu() -> GpuInfo {
    GpuInfo {
        name: "Unavailable".into(),
        renderer: "unknown".into(),
        driver: "unknown".into(),
        api: "DRM/KMS".into(),
    }
}

fn read_gpu_temperature(device: &Path) -> u16 {
    let Ok(entries) = fs::read_dir(device.join("hwmon")) else {
        return 0;
    };
    entries
        .flatten()
        .find_map(|entry| {
            read_trimmed(entry.path().join("temp1_input"))
                .and_then(|value| value.parse::<u64>().ok())
                .map(|value| (value / 1000) as u16)
        })
        .unwrap_or(0)
}

fn parse_key_values(contents: &str, separator: char) -> HashMap<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(separator)?;
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

fn read_journal_logs() -> Vec<LogEntry> {
    let output = Command::new("journalctl")
        .args([
            "--user",
            "--no-pager",
            "--output=json",
            "--lines=80",
            "--unit=focaldesk-server.service",
            "--unit=focaldesk-desktop.service",
            "--unit=focaldesk-powerd.service",
            "--unit=focaldesk-notificationsd.service",
            "--unit=focaldesk-controlsd.service",
            "--unit=focald-secrets.service",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let value: Value = serde_json::from_str(line).ok()?;
            let unit = value
                .get("_SYSTEMD_USER_UNIT")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let identifier = value
                .get("SYSLOG_IDENTIFIER")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !unit.starts_with("focal") && !identifier.starts_with("focal") {
                return None;
            }
            let message = value
                .get("MESSAGE")
                .and_then(Value::as_str)?
                .chars()
                .take(2_000)
                .collect::<String>();
            let timestamp = value
                .get("__REALTIME_TIMESTAMP")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<i64>().ok())
                .unwrap_or_default();
            let date = Local
                .timestamp_micros(timestamp)
                .single()
                .unwrap_or_else(Local::now);
            let priority = value
                .get("PRIORITY")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<u8>().ok())
                .unwrap_or(6);
            Some(LogEntry {
                id: timestamp.max(0) as u64,
                time: date.format("%H:%M:%S").to_string(),
                level: if priority <= 3 {
                    "error"
                } else if priority == 4 {
                    "warn"
                } else {
                    "info"
                }
                .into(),
                source: if !unit.is_empty() {
                    unit.trim_end_matches(".service").to_string()
                } else {
                    identifier.to_string()
                },
                message,
            })
        })
        .rev()
        .take(50)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_interval_is_bounded() {
        assert_eq!(clamp_interval(0), 1_000);
        assert_eq!(clamp_interval(5_000), 5_000);
        assert_eq!(clamp_interval(100_000), 30_000);
    }

    #[test]
    fn protocol_request_uses_the_existing_versioned_envelope() {
        let bytes = br#"{"protocol_version":1,"payload":{"type":"GetSnapshot"}}"#;
        assert!(matches!(
            transport::decode_message::<ControlCenterRequest>(bytes),
            Ok(ControlCenterRequest::GetSnapshot)
        ));
    }

    #[test]
    fn uptime_format_is_stable() {
        assert_eq!(format_uptime(90_061), "1d 1h 1m");
    }
}
