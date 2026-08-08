use chrono::Utc;
use focaldesk_logging::{crash_report_path_candidates, log_file_path_candidates};
use regex::Regex;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;

const MAX_ARTIFACT_BYTES: usize = 512 * 1024;
const MAX_TOTAL_BYTES: usize = 3 * 1024 * 1024;
const MAX_LOG_BYTES: usize = 512 * 1024;
const TRUNCATED_MARKER: &str = "\n[diagnostic output truncated]\n";

#[derive(Debug, Clone)]
pub struct DiagnosticsOptions {
    pub output: PathBuf,
    pub include_logs: bool,
}

impl Default for DiagnosticsOptions {
    fn default() -> Self {
        Self {
            output: default_output_path(),
            include_logs: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsReport {
    pub path: PathBuf,
    pub artifact_count: usize,
    pub uncompressed_bytes: usize,
}

#[derive(Default)]
struct Collector {
    artifacts: BTreeMap<String, Vec<u8>>,
    total_bytes: usize,
}

impl Collector {
    fn add(&mut self, name: &str, contents: impl AsRef<str>) {
        if self.total_bytes >= MAX_TOTAL_BYTES {
            return;
        }
        let redacted = redact_text(contents.as_ref());
        let remaining = MAX_TOTAL_BYTES - self.total_bytes;
        let limit = remaining.min(MAX_ARTIFACT_BYTES);
        let bounded = truncate_text(&redacted, limit);
        self.total_bytes += bounded.len();
        self.artifacts
            .insert(name.to_string(), bounded.into_bytes());
    }
}

pub fn default_output_path() -> PathBuf {
    let timestamp = Utc::now().format("%Y%m%d-%H%M%S");
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(format!("focaldesk-diagnostics-{timestamp}.tar.gz"))
}

pub fn collect_diagnostics(options: &DiagnosticsOptions) -> io::Result<DiagnosticsReport> {
    if options.output.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "diagnostics archive already exists: {}",
                options.output.display()
            ),
        ));
    }

    let mut collector = Collector::default();
    collector.add("system.txt", system_information());
    collector.add("session.txt", session_information());
    collector.add("graphics.txt", graphics_information());
    collector.add("services.txt", service_information());

    if options.include_logs {
        collector.add(
            "user-journal.txt",
            command_output(
                "journalctl",
                &[
                    "--user",
                    "--boot",
                    "--no-pager",
                    "--output=short-iso",
                    "--lines=500",
                    "--unit=focaldesk-session.target",
                    "--unit=focaldesk-server.service",
                    "--unit=focaldesk-desktop.service",
                    "--unit=focaldesk-powerd.service",
                    "--unit=focaldesk-notificationsd.service",
                    "--unit=focaldesk-controlsd.service",
                    "--unit=focaldesk-dialogd.service",
                    "--unit=focaldesk-portald.service",
                    "--unit=focal-launchd.service",
                ],
            ),
        );
        collector.add(
            "compositor-journal.txt",
            command_output(
                "journalctl",
                &[
                    "--boot",
                    "--no-pager",
                    "--output=short-iso",
                    "--lines=500",
                    "--identifier=focaldesk",
                ],
            ),
        );
        add_existing_file(
            &mut collector,
            "focaldesk.log",
            &log_file_path_candidates(),
            MAX_LOG_BYTES,
        );
        add_existing_file(
            &mut collector,
            "latest-crash.txt",
            &crash_report_path_candidates(),
            MAX_LOG_BYTES,
        );
    }

    let included = collector
        .artifacts
        .keys()
        .cloned()
        .collect::<Vec<_>>()
        .join("\n- ");
    collector.add(
        "README.txt",
        format!(
            "FocalDesk diagnostic bundle\n\
             Generated: {}\n\
             Schema: 1\n\
             Logs included: {}\n\
             \nThis bundle is bounded and automatically redacts common credential shapes, \
             home-directory paths, usernames, and hostnames. Redaction is best effort. \
             Review every file before sharing it publicly.\n\
             \nIncluded artifacts:\n- {included}\n",
            Utc::now().to_rfc3339(),
            options.include_logs,
        ),
    );

    write_archive(&options.output, &collector.artifacts)?;
    Ok(DiagnosticsReport {
        path: options.output.clone(),
        artifact_count: collector.artifacts.len(),
        uncompressed_bytes: collector.total_bytes,
    })
}

fn add_existing_file(
    collector: &mut Collector,
    artifact_name: &str,
    candidates: &[PathBuf],
    max_bytes: usize,
) {
    let Some(path) = candidates.iter().find(|path| path.is_file()) else {
        collector.add(artifact_name, "No matching file was found.\n");
        return;
    };
    match read_file_tail(path, max_bytes) {
        Ok(contents) => collector.add(artifact_name, contents),
        Err(error) => collector.add(
            artifact_name,
            format!("Could not read {}: {error}\n", path.display()),
        ),
    }
}

fn write_archive(output: &Path, artifacts: &BTreeMap<String, Vec<u8>>) -> io::Result<()> {
    let parent = output
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".focaldesk-diagnostics-")
        .tempdir_in(parent)?;
    for (name, contents) in artifacts {
        let path = staging.path().join(name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(contents)?;
        file.sync_all()?;
    }

    let archive = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(output)?;
    let tar = system_program("tar");
    let mut command = timed_command(&tar, "15s");
    let result = command
        .args(["--create", "--gzip", "--directory"])
        .arg(staging.path())
        .arg(".")
        .stdin(Stdio::null())
        .stdout(Stdio::from(archive))
        .stderr(Stdio::piped())
        .output();
    match result {
        Ok(result) if result.status.success() => Ok(()),
        Ok(result) => {
            let _ = fs::remove_file(output);
            Err(io::Error::other(format!(
                "tar failed: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            )))
        }
        Err(error) => {
            let _ = fs::remove_file(output);
            Err(io::Error::new(
                error.kind(),
                format!("could not run tar: {error}"),
            ))
        }
    }
}

fn system_information() -> String {
    let os_release = fs::read_to_string("/etc/os-release").unwrap_or_default();
    let wanted = ["PRETTY_NAME", "ID", "VERSION_ID"];
    let os = os_release
        .lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(key, _)| wanted.contains(key))
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("\n");
    let cpu = fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|line| {
                let (key, value) = line.split_once(':')?;
                (key.trim() == "model name").then(|| value.trim().to_string())
            })
        })
        .unwrap_or_else(|| "unknown".to_string());
    let memory = fs::read_to_string("/proc/meminfo")
        .unwrap_or_default()
        .lines()
        .filter(|line| line.starts_with("MemTotal:") || line.starts_with("SwapTotal:"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "FocalDesk CLI version: {}\nBuild hash: {}\nBuild profile: {}\nOS: {}\nArch: {}\n\
         {os}\nKernel: {}\nCPU: {cpu}\n{memory}\nUptime: {}\n",
        env!("CARGO_PKG_VERSION"),
        option_env!("VERGEN_GIT_SHA").unwrap_or("development"),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        },
        std::env::consts::OS,
        std::env::consts::ARCH,
        command_output("uname", &["-srmo"]).trim(),
        fs::read_to_string("/proc/uptime")
            .unwrap_or_else(|_| "unavailable".to_string())
            .trim(),
    )
}

fn session_information() -> String {
    let keys = [
        "XDG_SESSION_TYPE",
        "XDG_SESSION_CLASS",
        "XDG_SESSION_DESKTOP",
        "XDG_CURRENT_DESKTOP",
        "DESKTOP_SESSION",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "GDK_BACKEND",
        "RUST_LOG",
        "FOCALDESK_LOG",
    ];
    let mut output = String::new();
    for key in keys {
        let value = std::env::var(key).unwrap_or_else(|_| "unset".to_string());
        output.push_str(&format!("{key}={value}\n"));
    }
    output.push_str(&format!(
        "current_executable={}\n",
        std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|_| "unknown".to_string())
    ));
    output
}

fn graphics_information() -> String {
    let mut lines = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return "DRM sysfs is unavailable.\n".to_string();
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        let path = entry.path();
        if name.starts_with("card") && !name.contains('-') {
            let values = fs::read_to_string(path.join("device/uevent"))
                .unwrap_or_default()
                .lines()
                .filter(|line| {
                    line.starts_with("DRIVER=")
                        || line.starts_with("PCI_ID=")
                        || line.starts_with("PCI_SUBSYS_ID=")
                })
                .collect::<Vec<_>>()
                .join(" ");
            lines.push(format!("GPU {name}: {values}"));
        } else if name.starts_with("card") && name.contains('-') {
            let status = read_trimmed(path.join("status")).unwrap_or_else(|| "unknown".into());
            let enabled = read_trimmed(path.join("enabled")).unwrap_or_else(|| "unknown".into());
            let modes = fs::read_to_string(path.join("modes"))
                .unwrap_or_default()
                .lines()
                .take(16)
                .collect::<Vec<_>>()
                .join(", ");
            lines.push(format!(
                "Connector {name}: status={status} enabled={enabled} modes=[{modes}]"
            ));
        }
    }
    if lines.is_empty() {
        "No DRM devices were reported.\n".to_string()
    } else {
        lines.join("\n") + "\n"
    }
}

fn service_information() -> String {
    let mut output = String::new();
    output.push_str("Failed user services:\n");
    output.push_str(&command_output(
        "systemctl",
        &["--user", "--no-pager", "--plain", "--failed"],
    ));
    output.push_str("\nFocalDesk user services:\n");
    output.push_str(&command_output(
        "systemctl",
        &[
            "--user",
            "--no-pager",
            "--plain",
            "--all",
            "list-units",
            "focaldesk-*",
            "focald-*",
            "focal-launchd.service",
        ],
    ));
    output.push_str("\nRuntime sockets:\n");
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|path| path.join("focaldesk"));
    for socket in [
        "desktop.sock",
        "settings.sock",
        "notifications.sock",
        "power.sock",
        "controls.sock",
        "dialog.sock",
        "focaldesk-ai.sock",
        "focal-launchd.sock",
    ] {
        let available = runtime
            .as_ref()
            .map(|root| root.join(socket))
            .and_then(|path| path.symlink_metadata().ok())
            .is_some_and(|metadata| metadata.file_type().is_socket());
        output.push_str(&format!(
            "{socket}: {}\n",
            if available {
                "available"
            } else {
                "unavailable"
            }
        ));
    }
    output
}

fn read_trimmed(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn command_output(program: &str, arguments: &[&str]) -> String {
    let program_path = system_program(program);
    match timed_command(&program_path, "5s")
        .args(arguments)
        .stdin(Stdio::null())
        .output()
    {
        Ok(output) => {
            let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
            if !output.status.success() {
                combined.push_str(&format!(
                    "\n[command exited with status {}]\n",
                    output.status
                ));
                if !output.stderr.is_empty() {
                    combined.push_str("stderr:\n");
                    combined.push_str(&String::from_utf8_lossy(&output.stderr));
                }
            }
            if combined.trim().is_empty() {
                format!("{program} returned no output (status {}).\n", output.status)
            } else {
                truncate_text(&combined, MAX_ARTIFACT_BYTES)
            }
        }
        Err(error) => format!("Could not run {program}: {error}\n"),
    }
}

fn system_program(name: &str) -> PathBuf {
    ["/usr/bin", "/bin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

fn timed_command(program: &Path, duration: &str) -> Command {
    let timeout = system_program("timeout");
    if timeout.is_file() {
        let mut command = Command::new(timeout);
        command.arg("--signal=KILL").arg(duration).arg(program);
        command
    } else {
        Command::new(program)
    }
}

fn read_file_tail(path: &Path, max_bytes: usize) -> io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let start = length.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((length - start) as usize);
    file.read_to_end(&mut bytes)?;
    if start > 0 {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        }
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

pub fn redact_text(contents: &str) -> String {
    let mut redacted = contents
        .lines()
        .map(redact_line)
        .collect::<Vec<_>>()
        .join("\n");
    if contents.ends_with('\n') {
        redacted.push('\n');
    }
    for value in [
        std::env::var("HOME").ok(),
        std::env::var("USER").ok(),
        std::env::var("LOGNAME").ok(),
        std::env::var("HOSTNAME").ok(),
    ]
    .into_iter()
    .flatten()
    .filter(|value| value.len() >= 3)
    {
        redacted = redacted.replace(&value, "[PRIVATE]");
    }
    redacted
}

fn redact_line(line: &str) -> String {
    if let Ok(mut json) = serde_json::from_str::<Value>(line) {
        redact_json(&mut json);
        return serde_json::to_string(&json).unwrap_or_else(|_| "[REDACTED]".to_string());
    }
    static BEARER: OnceLock<Regex> = OnceLock::new();
    static QUOTED_ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    let bearer = BEARER
        .get_or_init(|| Regex::new(r"(?i)\bbearer[\s:]+[^\s,;}\]]+").expect("valid bearer regex"));
    let quoted = QUOTED_ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(password|passwd|secret|token|authorization|api[_-]?key|access[_-]?key|private[_-]?key|credential)\b[\"']?\s*[:=]\s*(\"[^\"]*\"|'[^']*')"#,
        )
        .expect("valid quoted credential regex")
    });
    let assignment = ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r#"(?i)\b(password|passwd|secret|token|authorization|api[_-]?key|access[_-]?key|private[_-]?key|credential)\b[\"']?\s*[:=]\s*[\"']?[^\s,;}\]]+"#,
        )
        .expect("valid credential regex")
    });
    let redacted = bearer.replace_all(line, "Bearer [REDACTED]");
    let redacted = quoted.replace_all(&redacted, "$1=[REDACTED]");
    assignment
        .replace_all(&redacted, "$1=[REDACTED]")
        .into_owned()
}

fn redact_json(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_sensitive_key(key) {
                    *value = Value::String("[REDACTED]".to_string());
                } else {
                    redact_json(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_json),
        _ => {}
    }
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    [
        "password",
        "passwd",
        "secret",
        "token",
        "authorization",
        "apikey",
        "accesskey",
        "privatekey",
        "credential",
    ]
    .iter()
    .any(|sensitive| normalized == *sensitive || normalized.ends_with(sensitive))
}

fn truncate_text(contents: &str, max_bytes: usize) -> String {
    if contents.len() <= max_bytes {
        return contents.to_string();
    }
    let marker_len = TRUNCATED_MARKER.len().min(max_bytes);
    let mut end = max_bytes.saturating_sub(marker_len);
    while !contents.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = contents[..end].to_string();
    output.push_str(&TRUNCATED_MARKER[..marker_len]);
    output
}

#[cfg(test)]
mod tests {
    use super::{read_file_tail, redact_text, truncate_text};
    use std::fs;

    #[test]
    fn redacts_credentials_in_text_and_json() {
        let text = concat!(
            "token=abc password='hunter two' status=failed\n",
            r#"{"request":{"api_key":"def","safe":"visible"}}"#,
        );
        let redacted = redact_text(text);
        assert!(!redacted.contains("abc"));
        assert!(!redacted.contains("hunter two"));
        assert!(!redacted.contains("def"));
        assert!(redacted.contains("visible"));
        assert!(redacted.contains("[REDACTED]"));
    }

    #[test]
    fn truncation_respects_utf8_boundaries() {
        let truncated = truncate_text("abcdef😀ghijkl", 12);
        assert!(truncated.len() <= 12);
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn tail_reader_drops_a_partial_first_line() {
        let path =
            std::env::temp_dir().join(format!("focaldesk-diagnostics-tail-{}", std::process::id()));
        fs::write(&path, "first line\nsecond line\nthird line\n").unwrap();
        let tail = read_file_tail(&path, 20).unwrap();
        assert_eq!(tail, "third line\n");
        let _ = fs::remove_file(path);
    }
}
