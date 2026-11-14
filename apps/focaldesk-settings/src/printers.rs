//! Typed CUPS controls, following the existing GTK backend's CLI protocol.
use crate::control_process::run;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Mutex,
    time::Duration,
};
use url::Url;

#[derive(Clone, Serialize)]
pub struct Printer {
    pub name: String,
    pub enabled: bool,
    pub accepting_jobs: Option<bool>,
    pub is_default: bool,
    pub state: String,
    pub device_uri: Option<String>,
}
#[derive(Clone, Serialize)]
pub struct DiscoveredPrinter {
    pub id: String,
    pub kind: String,
    pub uri: String,
    pub suggested_name: String,
}
struct Device {
    id: String,
    kind: String,
    uri: String,
    name: String,
}
#[derive(Clone, Serialize)]
pub struct Driver {
    pub id: String,
    pub name: String,
}
#[derive(Clone, Serialize)]
pub struct PrintJob {
    pub id: String,
    pub printer: String,
    pub owner: String,
    pub bytes: u64,
    pub submitted: String,
}
#[derive(Serialize)]
pub struct PrinterSnapshot {
    pub scheduler_running: bool,
    pub printers: Vec<Printer>,
    pub discovered: Vec<DiscoveredPrinter>,
    pub drivers: Vec<Driver>,
    pub warnings: Vec<String>,
}
#[derive(Serialize)]
pub struct PrinterResult {
    pub snapshot: PrinterSnapshot,
    pub message: String,
}
#[derive(Default)]
struct State {
    discovered: Vec<Device>,
    drivers: Vec<Driver>,
}
#[derive(Default)]
pub struct PrinterService {
    state: Mutex<State>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum PrinterAction {
    Discover,
    LoadDrivers,
    Install {
        name: String,
        uri: String,
        model: String,
    },
    InstallDiscovered {
        id: String,
        name: String,
        model: String,
    },
    SetDefault {
        name: String,
    },
    SetEnabled {
        name: String,
        enabled: bool,
    },
    SetAccepting {
        name: String,
        accepting: bool,
    },
    CancelJob {
        printer: String,
        id: String,
    },
}
fn cups(program: &str, args: &[&str], seconds: u64) -> Result<String, String> {
    run(program, args, None, Duration::from_secs(seconds))
}
fn empty_cups(args: &[&str], empty_markers: &[&str]) -> Result<String, String> {
    match cups("lpstat", args, 4) {
        Ok(value) => Ok(value),
        Err(error)
            if empty_markers
                .iter()
                .any(|marker| error.to_lowercase().contains(marker)) =>
        {
            Ok(String::new())
        }
        Err(error) => Err(error),
    }
}
pub fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 127
        && !value.starts_with('-')
        && value
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}
fn standard_backend(scheme: &str) -> bool {
    matches!(
        scheme,
        "ipp"
            | "ipps"
            | "http"
            | "https"
            | "socket"
            | "lpd"
            | "usb"
            | "dnssd"
            | "smb"
            | "serial"
            | "parallel"
    )
}
fn validate_device_uri(value: &str, registered_vendor: bool) -> Result<Url, String> {
    if value.len() > 2048
        || value
            .chars()
            .any(|ch| ch.is_control() || ch.is_whitespace())
    {
        return Err("Enter a printer URI without spaces or control characters.".into());
    }
    let url = Url::parse(value).map_err(|_| "Enter a valid printer device URI.")?;
    if (!standard_backend(url.scheme()) && !registered_vendor)
        || matches!(
            url.scheme(),
            "file" | "pipe" | "exec" | "beh" | "javascript" | "data" | "mailto"
        )
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
        || url.query_pairs().any(|(key, _)| {
            matches!(
                key.to_ascii_lowercase().as_str(),
                "password" | "passwd" | "token" | "access_token" | "username" | "user"
            )
        })
    {
        return Err("Use a supported printer URI without embedded credentials.".into());
    }
    if (standard_backend(url.scheme())
        && !matches!(url.scheme(), "serial" | "parallel")
        && url.host_str().is_none())
        || (!standard_backend(url.scheme())
            && url.host_str().is_none()
            && !url.path().starts_with('/'))
    {
        return Err("The printer URI needs a device or host name.".into());
    }
    Ok(url)
}
fn display_uri(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return "Device address unavailable".into();
    };
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}
fn suggested_name(uri: &str) -> String {
    let candidate = Url::parse(uri)
        .ok()
        .and_then(|url| {
            url.path_segments()
                .and_then(|mut parts| parts.rfind(|part| !part.is_empty()))
                .map(str::to_owned)
                .or_else(|| url.host_str().map(str::to_owned))
        })
        .unwrap_or_else(|| "Printer".into())
        .replace("%20", " ");
    let mut name = String::new();
    for ch in candidate.chars() {
        if ch.is_ascii_alphanumeric() {
            name.push(ch);
        } else if !name.ends_with('_') {
            name.push('_');
        }
    }
    let name = name.trim_matches('_').chars().take(100).collect::<String>();
    if name.is_empty() {
        "Printer".into()
    } else {
        name
    }
}
fn devices(output: &str) -> BTreeMap<String, String> {
    output
        .lines()
        .filter_map(|line| {
            let (name, uri) = line.strip_prefix("device for ")?.split_once(':')?;
            Some((name.trim().to_owned(), uri.trim().to_owned()))
        })
        .collect()
}
fn configured(output: &str, uris: &BTreeMap<String, String>) -> Vec<Printer> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let name = line.strip_prefix("printer ")?.split_whitespace().next()?;
            if !valid_name(name) {
                return None;
            }
            let enabled = !line.contains(" disabled ");
            let state = line
                .split_once(" is ")
                .map(|(_, rest)| rest.split('.').next().unwrap_or(rest))
                .unwrap_or("unknown")
                .trim()
                .to_owned();
            Some(Printer {
                name: name.into(),
                enabled,
                accepting_jobs: None,
                is_default: false,
                state,
                device_uri: uris.get(name).map(|uri| display_uri(uri)),
            })
        })
        .collect()
}
fn discovery(output: &str, configured: &BTreeMap<String, String>) -> Vec<Device> {
    let mut seen = BTreeSet::new();
    output
        .lines()
        .filter_map(|line| {
            let (kind, uri) = line.trim().split_once(char::is_whitespace)?;
            let uri = uri.trim();
            if validate_device_uri(uri, true).is_err()
                || configured.values().any(|known| known == uri)
                || !seen.insert(uri.to_owned())
            {
                return None;
            }
            Some(Device {
                id: format!("{:x}", Sha256::digest(uri.as_bytes())),
                kind: kind.into(),
                uri: uri.into(),
                name: suggested_name(uri),
            })
        })
        .collect()
}
fn driver_list() -> Result<Vec<Driver>, String> {
    Ok(cups("lpinfo", &["-m"], 8)?
        .lines()
        .filter_map(|line| {
            let (id, name) = line.split_once(char::is_whitespace)?;
            (!id.starts_with('-') && !id.chars().any(char::is_control)).then(|| Driver {
                id: id.into(),
                name: name.trim().into(),
            })
        })
        .collect())
}
fn job_list(printer: &str) -> Result<Vec<PrintJob>, String> {
    if !valid_name(printer) {
        return Err("Invalid printer name.".into());
    }
    let output = empty_cups(
        &["-W", "not-completed", "-o", printer],
        &["no entries", "no destinations added"],
    )?;
    Ok(parse_jobs(&output, printer))
}
fn parse_jobs(output: &str, printer: &str) -> Vec<PrintJob> {
    output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let id = fields.next()?;
            let (queue, number) = id.rsplit_once('-')?;
            if queue != printer || number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            Some(PrintJob {
                id: id.into(),
                printer: queue.into(),
                owner: fields.next()?.into(),
                bytes: fields.next()?.parse().ok()?,
                submitted: fields.collect::<Vec<_>>().join(" "),
            })
        })
        .collect()
}
fn snapshot(state: &mut State) -> Result<PrinterSnapshot, String> {
    let running = empty_cups(&["-r"], &["scheduler is not running"])?
        .to_ascii_lowercase()
        .contains("scheduler is running");
    if !running {
        return Ok(PrinterSnapshot {
            scheduler_running: false,
            printers: vec![],
            discovered: vec![],
            drivers: state.drivers.clone(),
            warnings: vec![],
        });
    }
    let uris = devices(&empty_cups(&["-v"], &["no destinations added"])?);
    let mut printers = configured(&empty_cups(&["-p"], &["no destinations added"])?, &uris);
    let mut warnings = Vec::new();
    match empty_cups(&["-a"], &["no destinations added"]) {
        Ok(output) => {
            for line in output.lines() {
                let mut fields = line.split_whitespace();
                let first = fields.next().unwrap_or("");
                let name = if first == "printer" {
                    fields.next().unwrap_or("")
                } else {
                    first
                };
                if let Some(printer) = printers.iter_mut().find(|printer| printer.name == name) {
                    printer.accepting_jobs = Some(
                        line.contains(" accepting requests")
                            && !line.contains(" not accepting requests"),
                    );
                }
            }
        }
        Err(_) => warnings.push("Could not read whether queues accept new jobs.".into()),
    }
    match empty_cups(
        &["-d"],
        &["no system default destination", "no destinations added"],
    ) {
        Ok(output) => {
            let default = output
                .strip_prefix("system default destination:")
                .map(str::trim);
            for printer in &mut printers {
                printer.is_default = default == Some(printer.name.as_str());
            }
        }
        Err(_) => warnings.push("Could not read the default printer.".into()),
    }
    printers.sort_by(|a, b| b.is_default.cmp(&a.is_default).then(a.name.cmp(&b.name)));
    state
        .discovered
        .retain(|device| !uris.values().any(|uri| *uri == device.uri));
    let discovered = state
        .discovered
        .iter()
        .map(|device| DiscoveredPrinter {
            id: device.id.clone(),
            kind: device.kind.clone(),
            uri: display_uri(&device.uri),
            suggested_name: device.name.clone(),
        })
        .collect();
    Ok(PrinterSnapshot {
        scheduler_running: running,
        printers,
        discovered,
        drivers: state.drivers.clone(),
        warnings,
    })
}
fn install(
    state: &mut State,
    current: &PrinterSnapshot,
    name: &str,
    uri: &str,
    model: &str,
) -> Result<(), String> {
    if !valid_name(name) {
        return Err("Use a printer name of 1–127 bytes containing letters, numbers, underscores, dots, or hyphens, without a leading hyphen.".into());
    }
    if current
        .printers
        .iter()
        .any(|printer| printer.name.eq_ignore_ascii_case(name))
    {
        return Err("A printer with this name already exists. Choose another name.".into());
    }
    let device = validate_device_uri(uri, true)?;
    if !standard_backend(device.scheme()) {
        let output = cups("lpinfo", &["-v"], 10)?;
        let registered = output.lines().any(|line| {
            line.trim()
                .split_once(char::is_whitespace)
                .is_some_and(|(_, value)| {
                    let value = value.trim();
                    value == device.scheme()
                        || Url::parse(value).is_ok_and(|known| known.scheme() == device.scheme())
                })
        });
        if !registered {
            return Err("This printer backend is not registered with the print service.".into());
        }
    }
    if model.len() > 512 || model.starts_with('-') || model.chars().any(char::is_control) {
        return Err("Invalid printer driver identifier.".into());
    }
    let model = if model.is_empty() {
        "everywhere"
    } else {
        model
    };
    if model != "everywhere" && model != "raw" {
        state.drivers = driver_list()?;
        if !state.drivers.iter().any(|driver| driver.id == model) {
            return Err("Choose a driver installed on your print service.".into());
        }
    }
    cups("lpadmin", &["-p", name, "-E", "-v", uri, "-m", model], 12)?;
    Ok(())
}
impl PrinterService {
    pub fn snapshot(&self) -> Result<PrinterSnapshot, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Printer controls unavailable.")?;
        snapshot(&mut state)
    }
    pub fn jobs(&self, name: String) -> Result<Vec<PrintJob>, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Printer controls unavailable.")?;
        let current = snapshot(&mut state)?;
        if !current.printers.iter().any(|printer| printer.name == name) {
            return Err("This printer disappeared. Refresh before continuing.".into());
        }
        job_list(&name)
    }
    pub fn apply(&self, action: PrinterAction) -> Result<PrinterResult, String> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Printer controls unavailable.")?;
        let current = snapshot(&mut state)?;
        if !current.scheduler_running {
            return Err("The CUPS print service is not running.".into());
        }
        let message = match action {
            PrinterAction::Discover => {
                let uris = devices(&empty_cups(&["-v"], &["no destinations added"])?);
                state.discovered = discovery(&cups("lpinfo", &["-v"], 10)?, &uris);
                "Printer discovery complete."
            }
            PrinterAction::LoadDrivers => {
                state.drivers = driver_list()?;
                "Installed drivers loaded."
            }
            PrinterAction::Install { name, uri, model } => {
                install(&mut state, &current, &name, &uri, &model)?;
                "Printer installation requested. Current queues are shown below."
            }
            PrinterAction::InstallDiscovered { id, name, model } => {
                let uris = devices(&empty_cups(&["-v"], &["no destinations added"])?);
                let fresh = discovery(&cups("lpinfo", &["-v"], 10)?, &uris);
                let device = fresh
                    .iter()
                    .find(|device| device.id == id)
                    .ok_or("This discovered printer disappeared. Find printers again.")?;
                install(&mut state, &current, &name, &device.uri, &model)?;
                "Printer installation requested. Current queues are shown below."
            }
            PrinterAction::CancelJob { printer, id } => {
                if !current.printers.iter().any(|queue| queue.name == printer) {
                    return Err("This printer disappeared. Refresh before continuing.".into());
                }
                if !job_list(&printer)?.iter().any(|job| job.id == id) {
                    return Err(
                        "This job already finished or disappeared. Refresh the queue.".into(),
                    );
                }
                cups("cancel", &[&id], 8)?;
                "Job cancellation requested. Pages already printed cannot be recalled."
            }
            action => {
                let name = match &action {
                    PrinterAction::SetDefault { name }
                    | PrinterAction::SetEnabled { name, .. }
                    | PrinterAction::SetAccepting { name, .. } => name,
                    _ => unreachable!(),
                };
                let printer = current
                    .printers
                    .iter()
                    .find(|printer| printer.name == *name)
                    .ok_or("This printer disappeared. Refresh before continuing.")?;
                match action {
                    PrinterAction::SetDefault { .. } => {
                        cups("lpadmin", &["-d", &printer.name], 8)?;
                        "Default printer updated on the print service."
                    }
                    PrinterAction::SetEnabled { enabled, .. } => {
                        cups(
                            if enabled { "cupsenable" } else { "cupsdisable" },
                            &[&printer.name],
                            8,
                        )?;
                        if enabled {
                            "Printer processing resumed."
                        } else {
                            "Printer processing paused."
                        }
                    }
                    PrinterAction::SetAccepting { accepting, .. } => {
                        cups(
                            if accepting {
                                "cupsaccept"
                            } else {
                                "cupsreject"
                            },
                            &[&printer.name],
                            8,
                        )?;
                        if accepting {
                            "Queue now accepts new jobs."
                        } else {
                            "Queue now rejects new jobs."
                        }
                    }
                    _ => unreachable!(),
                }
            }
        };
        let snapshot = snapshot(&mut state).map_err(|error| {
            format!("The printer operation completed, but refreshing status failed: {error}")
        })?;
        Ok(PrinterResult {
            snapshot,
            message: message.into(),
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_names_and_device_uris_and_redacts_credentials() {
        assert!(valid_name("Office_Printer-2"));
        assert!(valid_name("办公室"));
        for value in ["-d", "../../x", "a b", ""] {
            assert!(!valid_name(value));
        }
        assert!(validate_device_uri("ipp://printer.local/ipp/print", false).is_ok());
        assert!(validate_device_uri("usb://Vendor/Model?serial=123", false).is_ok());
        assert!(validate_device_uri("vendorprint:/usb/Model", false).is_err());
        assert!(validate_device_uri("vendorprint:/usb/Model", true).is_ok());
        assert!(validate_device_uri("file:///tmp/out", true).is_err());
        for uri in [
            "file:///tmp/out",
            "beh:/bin/sh",
            "ipp://user:secret@printer.local/ipp/print",
            "ipp://printer.local/ipp/print?password=secret",
        ] {
            assert!(validate_device_uri(uri, false).is_err());
        }
        let caption = display_uri("smb://user:secret@server/queue?password=hidden");
        assert!(!caption.contains("secret"));
        assert!(!caption.contains("hidden"));
    }
    #[test]
    fn parses_cups_queues_and_job_ids_with_hyphenated_names() {
        let uris = devices("device for Office-Printer: ipp://printer.local/ipp/print\n");
        let queues=configured("printer Office-Printer is idle. enabled since yesterday\nprinter Paused disabled since yesterday -",&uris);
        assert_eq!(queues.len(), 2);
        assert!(queues[0].enabled);
        assert!(!queues[1].enabled);
        let jobs=parse_jobs("Office-Printer-123 alice 4096 Thu Nov 12 09:00:00\nOther-124 bob 10 now\n--help bad 0 now","Office-Printer");
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "Office-Printer-123");
        assert!(
            discovery(
                "network ipp\nnetwork ipp://new.local/ipp/print\nnetwork ipp://new.local/ipp/print",
                &uris
            )
            .len()
                == 1
        );
    }
}
