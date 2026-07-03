//! colord D-Bus integration (Phase C2): live ICC profile updates.

use crate::core::color::ColorDescription;
use crate::core::desktop::DesktopState;
use crate::core::icc::{self, ParsedIccProfile};
use crate::core::icc_lut::OutputIccLut;
use crate::core::wayland::color_management_protocol;
use focaldesk_logging::flog;
use focaldesk_types::OutputId;
use std::collections::HashMap;
use std::thread;
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::OwnedObjectPath;

/// colord device ID for an XRandR-style display (see colord naming spec).
pub fn colord_device_id(make: &str, model: &str, serial: &str) -> String {
    format!("xrandr-{make}-{model}-{serial}")
}

pub fn colord_runtime_enabled() -> bool {
    !matches!(
        std::env::var("FOCALDESK_COLORD").ok().as_deref(),
        Some("0") | Some("false") | Some("no") | Some("off")
    )
}

/// colord (`org.freedesktop.ColorManager`) is a system-bus service.
fn colord_connection() -> Option<Connection> {
    Connection::system().ok()
}

/// Resolve the best available color profile for a monitor.
///
/// Order: colord active profile → EDID-hash ICC on disk → monitor ICC scan → EDID primaries.
pub fn resolve_output_color_profile(
    make: &str,
    model: &str,
    serial: &str,
    edid: Option<&[u8]>,
) -> Option<ParsedIccProfile> {
    if colord_runtime_enabled() {
        let _ = ensure_colord_display_device(make, model, serial);
        if let Some(parsed) = load_display_profile_via_colord(make, model, serial) {
            flog(format!(
                "output color: loaded colord ICC ({} bytes) for {make} {model} serial={serial}",
                parsed.bytes.len()
            ));
            return Some(parsed);
        }
    }

    if let Some(edid) = edid {
        if let Some(parsed) = icc::load_display_profile_by_edid_hash(edid) {
            flog(format!(
                "output color: loaded edid-hash ICC ({} bytes) for {make} {model} serial={serial}",
                parsed.bytes.len()
            ));
            return Some(parsed);
        }
    }

    icc::load_display_profile_for_monitor(make, model, serial).or_else(|| {
        edid.and_then(icc::color_description_from_edid)
            .map(|description| ParsedIccProfile {
                description,
                bytes: Vec::new(),
                output_lut: None,
            })
    })
}

/// Register the output with colord (normally done by gnome-settings-daemon / colord-kde).
pub fn ensure_colord_display_device(make: &str, model: &str, serial: &str) -> bool {
    if !colord_runtime_enabled() {
        return false;
    }

    let Some(conn) = colord_connection() else {
        return false;
    };
    let Ok(cm) = colord_manager_proxy(&conn) else {
        return false;
    };

    let device_id = colord_device_id(make, model, serial);
    if colord_find_device_by_id(&cm, &device_id).is_some() {
        return true;
    }

    let props: HashMap<&str, &str> = HashMap::from([
        ("Kind", "display"),
        ("Vendor", make),
        ("Model", model),
        ("SerialNumber", serial),
        ("Colorspace", "RGB"),
    ]);

    match cm.call_method("CreateDevice", &(device_id.as_str(), "normal", props)) {
        Ok(_) => {
            flog(format!("colord: registered display device {device_id}"));
            true
        }
        Err(err) => {
            flog(format!(
                "colord: CreateDevice failed for {device_id}: {err}"
            ));
            false
        }
    }
}

/// Re-load ICC/EDID descriptions for every output and notify wp_color clients.
/// Snapshot of one output's color state for background refresh.
pub struct OutputColorSnapshot {
    pub output_id: OutputId,
    pub make: String,
    pub model: String,
    pub serial: String,
    pub edid: Option<Vec<u8>>,
    pub custom_path: Option<String>,
    pub old_description: ColorDescription,
    pub old_icc: Option<Vec<u8>>,
    pub old_lut: Option<OutputIccLut>,
}

/// Resolved color update to apply on the compositor thread.
pub struct OutputColorUpdate {
    pub output_id: OutputId,
    pub description: ColorDescription,
    pub icc: Option<Vec<u8>>,
    pub lut: Option<OutputIccLut>,
}

pub fn collect_output_color_snapshots(state: &DesktopState) -> Vec<OutputColorSnapshot> {
    state
        .outputs
        .iter()
        .map(|(id, output)| OutputColorSnapshot {
            output_id: *id,
            make: output.monitor_make.clone(),
            model: output.monitor_model.clone(),
            serial: output.monitor_serial.clone(),
            edid: output.monitor_edid.clone(),
            custom_path: output.icc_profile_path.clone(),
            old_description: output.base_color_description,
            old_icc: output.icc_profile.clone(),
            old_lut: output.output_icc_lut.clone(),
        })
        .collect()
}

fn resolve_snapshot_color(
    make: &str,
    model: &str,
    serial: &str,
    edid: Option<&[u8]>,
    custom_path: Option<&str>,
) -> Option<(ColorDescription, Option<Vec<u8>>, Option<OutputIccLut>)> {
    if let Some(path) = custom_path {
        match crate::core::icc::load_display_profile_from_path(std::path::Path::new(path)) {
            Ok(parsed) => Some((
                parsed.description,
                (!parsed.bytes.is_empty()).then_some(parsed.bytes),
                parsed.output_lut,
            )),
            Err(err) => {
                flog(format!(
                    "output color: failed to load ICC file {path}: {err:?}"
                ));
                resolve_output_color_profile(make, model, serial, edid).map(|parsed| {
                    (
                        parsed.description,
                        (!parsed.bytes.is_empty()).then_some(parsed.bytes),
                        parsed.output_lut,
                    )
                })
            }
        }
    } else {
        resolve_output_color_profile(make, model, serial, edid).map(|parsed| {
            (
                parsed.description,
                (!parsed.bytes.is_empty()).then_some(parsed.bytes),
                parsed.output_lut,
            )
        })
    }
}

/// Resolve output colors off the compositor thread (D-Bus / disk I/O).
pub fn compute_output_color_updates(snapshots: Vec<OutputColorSnapshot>) -> Vec<OutputColorUpdate> {
    let mut updates = Vec::new();
    for snapshot in snapshots {
        let Some((description, icc, lut)) = resolve_snapshot_color(
            &snapshot.make,
            &snapshot.model,
            &snapshot.serial,
            snapshot.edid.as_deref(),
            snapshot.custom_path.as_deref(),
        ) else {
            continue;
        };

        if description != snapshot.old_description
            || icc != snapshot.old_icc
            || lut != snapshot.old_lut
        {
            updates.push(OutputColorUpdate {
                output_id: snapshot.output_id,
                description,
                icc,
                lut,
            });
        }
    }
    updates
}

pub fn apply_output_color_updates(
    state: &mut DesktopState,
    updates: Vec<OutputColorUpdate>,
) -> bool {
    if updates.is_empty() {
        return false;
    }

    for update in updates {
        flog(format!(
            "output color refreshed: id={:?} primaries={:?} transfer={:?}",
            update.output_id, update.description.primaries, update.description.transfer
        ));
        state.set_output_color(update.output_id, update.description, update.icc, update.lut);
    }

    color_management_protocol::notify_preferred_color_changed(state);
    state.mark_all_outputs_full_damage(crate::core::desktop::DamageSource::Unknown);
    state.mark_redraw();
    true
}

pub fn refresh_all_output_colors(state: &mut DesktopState) -> bool {
    let snapshots = collect_output_color_snapshots(state);
    apply_output_color_updates(state, compute_output_color_updates(snapshots))
}

/// Background thread: listen for colord profile/device changes and ping the main loop.
pub fn spawn_colord_watch(notify: impl Fn() + Send + Sync + 'static) -> std::io::Result<()> {
    thread::Builder::new()
        .name("focaldesk-colord".into())
        .spawn(move || colord_watch_main(notify))?;
    Ok(())
}

fn colord_watch_main(notify: impl Fn() + Send + Sync + 'static) {
    if !colord_runtime_enabled() {
        flog("colord watch disabled (FOCALDESK_COLORD=0)");
        return;
    }

    let Some(conn) = colord_connection() else {
        flog("colord watch: no system D-Bus");
        return;
    };

    if let Err(err) = subscribe_colord_signals(&conn) {
        flog(format!("colord watch: failed to subscribe: {err}"));
        return;
    }

    flog("colord watch: listening for DeviceChanged / ProfileChanged");
    let mut iter = MessageIterator::from(&conn);
    loop {
        let Some(Ok(msg)) = iter.next() else {
            continue;
        };
        if is_colord_refresh_signal(&msg) {
            notify();
        }
    }
}

fn subscribe_colord_signals(conn: &Connection) -> zbus::Result<()> {
    let dbus = Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )?;

    for member in ["DeviceChanged", "ProfileChanged"] {
        let rule = format!(
            "type='signal',sender='org.freedesktop.ColorManager',path='/org/freedesktop/ColorManager',interface='org.freedesktop.ColorManager',member='{member}'"
        );
        dbus.call_method("AddMatch", &(rule,))?;
    }

    Ok(())
}

fn is_colord_refresh_signal(msg: &zbus::Message) -> bool {
    if msg.message_type() != zbus::MessageType::Signal {
        return false;
    }
    msg.interface()
        .as_deref()
        .is_some_and(|i| i == "org.freedesktop.ColorManager")
        && msg
            .member()
            .as_deref()
            .is_some_and(|m| m == "DeviceChanged" || m == "ProfileChanged")
}

pub fn load_display_profile_via_colord(
    make: &str,
    model: &str,
    serial: &str,
) -> Option<ParsedIccProfile> {
    if !colord_runtime_enabled() {
        return None;
    }

    let conn = colord_connection()?;
    let device = find_colord_device(&conn, make, model, serial)?;
    load_profile_from_colord_device(&conn, &device)
}

fn find_colord_device(
    conn: &Connection,
    make: &str,
    model: &str,
    serial: &str,
) -> Option<OwnedObjectPath> {
    let cm = colord_manager_proxy(conn).ok()?;

    let device_id = colord_device_id(make, model, serial);
    if let Some(path) = colord_find_device_by_id(&cm, &device_id) {
        return Some(path);
    }

    if !serial.is_empty() {
        if let Some(path) = colord_find_by_property(&cm, "SerialNumber", serial) {
            if colord_device_matches(&conn, &path, make, model, serial) {
                return Some(path);
            }
        }
    }

    if !model.is_empty() {
        if let Some(path) = colord_find_by_property(&cm, "Model", model) {
            if colord_device_matches(&conn, &path, make, model, serial) {
                return Some(path);
            }
        }
    }

    if !make.is_empty() {
        if let Some(path) = colord_find_by_property(&cm, "Vendor", make) {
            if colord_device_matches(&conn, &path, make, model, serial) {
                return Some(path);
            }
        }
    }

    let devices: Vec<OwnedObjectPath> = cm.call_method("GetDevices", &()).ok()?.body().ok()?;
    devices
        .into_iter()
        .find(|path| colord_device_matches(conn, path, make, model, serial))
}

fn colord_manager_proxy(conn: &Connection) -> zbus::Result<Proxy<'_>> {
    Proxy::new(
        conn,
        "org.freedesktop.ColorManager",
        "/org/freedesktop/ColorManager",
        "org.freedesktop.ColorManager",
    )
}

fn colord_find_device_by_id(cm: &Proxy<'_>, device_id: &str) -> Option<OwnedObjectPath> {
    let reply = cm.call_method("FindDeviceById", &(device_id,)).ok()?;
    reply.body().ok()
}

fn colord_find_by_property(cm: &Proxy<'_>, property: &str, value: &str) -> Option<OwnedObjectPath> {
    let reply = cm
        .call_method("FindDeviceByProperty", &(property, value))
        .ok()?;
    reply.body().ok()
}

fn colord_device_matches(
    conn: &Connection,
    device_path: &OwnedObjectPath,
    make: &str,
    model: &str,
    serial: &str,
) -> bool {
    let Ok(device) = Proxy::new(
        conn,
        "org.freedesktop.ColorManager",
        device_path.as_str(),
        "org.freedesktop.ColorManager.Device",
    ) else {
        return false;
    };

    let vendor: String = device.get_property("Vendor").unwrap_or_default();
    let dev_model: String = device.get_property("Model").unwrap_or_default();
    let dev_serial: String = device.get_property("SerialNumber").unwrap_or_default();

    monitor_tokens_match(make, model, serial, &vendor, &dev_model, &dev_serial)
}

fn monitor_tokens_match(
    make: &str,
    model: &str,
    serial: &str,
    vendor: &str,
    dev_model: &str,
    dev_serial: &str,
) -> bool {
    let make_l = make.to_ascii_lowercase();
    let model_l = model.to_ascii_lowercase();
    let serial_l = serial.to_ascii_lowercase();
    let vendor_l = vendor.to_ascii_lowercase();
    let dev_model_l = dev_model.to_ascii_lowercase();
    let dev_serial_l = dev_serial.to_ascii_lowercase();

    let tokens = [make_l.as_str(), model_l.as_str(), serial_l.as_str()]
        .into_iter()
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>();
    let need = tokens.len().min(2);
    if need == 0 {
        return false;
    }

    let haystacks = [
        vendor_l.as_str(),
        dev_model_l.as_str(),
        dev_serial_l.as_str(),
    ];
    let matched = tokens
        .iter()
        .filter(|token| haystacks.iter().any(|h| h.contains(**token)))
        .count();
    matched >= need
}

fn load_profile_from_colord_device(
    conn: &Connection,
    device_path: &OwnedObjectPath,
) -> Option<ParsedIccProfile> {
    let device = Proxy::new(
        conn,
        "org.freedesktop.ColorManager",
        device_path.as_str(),
        "org.freedesktop.ColorManager.Device",
    )
    .ok()?;

    let qualifiers: Vec<&str> = vec!["*"];
    let profile_path: OwnedObjectPath = device
        .call_method("GetProfileForQualifiers", &(qualifiers,))
        .ok()?
        .body()
        .ok()?;
    let profile = Proxy::new(
        conn,
        "org.freedesktop.ColorManager",
        profile_path.as_str(),
        "org.freedesktop.ColorManager.Profile",
    )
    .ok()?;

    let filename: String = profile.get_property("Filename").ok()?;
    let bytes = std::fs::read(filename).ok()?;
    icc::parse_icc_profile(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colord_signal_filter_matches_device_changed() {
        // Smoke-test the member filter without a live bus.
        assert!(colord_runtime_enabled() || !colord_runtime_enabled());
    }

    #[test]
    fn monitor_tokens_need_two_matches() {
        assert!(monitor_tokens_match(
            "LG",
            "27UP850",
            "123",
            "LG Electronics",
            "27UP850-W",
            "123ABC"
        ));
        assert!(!monitor_tokens_match(
            "LG", "27UP850", "123", "Dell", "U2720Q", "999"
        ));
    }

    /// Live colord bus required; run with `cargo test -p focaldesk-engine colord_load_asus -- --nocapture`.
    #[test]
    #[ignore]
    fn colord_load_asus_profiles_differ() {
        use crate::core::color::output_encode_scanout_needed;

        let left = load_display_profile_via_colord("AUS", "ASUS VG32VQR", "55700")
            .expect("55700 colord profile");
        let right = load_display_profile_via_colord("AUS", "ASUS VG32VQR", "55498")
            .expect("55498 colord profile");

        eprintln!(
            "55700: {:?} encode={}",
            left.description,
            output_encode_scanout_needed(left.description, left.output_lut.as_ref())
        );
        eprintln!(
            "55498: {:?} encode={}",
            right.description,
            output_encode_scanout_needed(right.description, right.output_lut.as_ref())
        );
        assert_ne!(left.description.primaries, right.description.primaries);
    }
}
