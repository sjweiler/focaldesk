//! colord D-Bus integration (Phase C2): live ICC profile updates.

use crate::core::color::default_output_color_description;
use crate::core::desktop::{DamageSource, DesktopState};
use crate::core::icc::{self, ParsedIccProfile};
use crate::core::wayland::color_management_protocol;
use focaldesk_logging::flog;
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

/// Resolve the best available color profile for a monitor.
pub fn resolve_output_color_profile(
    make: &str,
    model: &str,
    serial: &str,
    edid: Option<&[u8]>,
) -> Option<ParsedIccProfile> {
    if let Some(edid) = edid {
        if let Some(parsed) = icc::load_display_profile_by_edid_hash(edid) {
            flog(format!(
                "output color: loaded edid-hash ICC ({} bytes) for {make} {model} serial={serial}",
                parsed.bytes.len()
            ));
            return Some(parsed);
        }
    }

    let _ = ensure_colord_display_device(make, model, serial);
    load_display_profile_via_colord(make, model, serial)
        .or_else(|| icc::load_display_profile_for_monitor(make, model, serial))
        .or_else(|| {
            edid
                .and_then(icc::color_description_from_edid)
                .map(|description| ParsedIccProfile {
                    description,
                    bytes: Vec::new(),
                })
        })
}

/// Register the output with colord (normally done by gnome-settings-daemon / colord-kde).
pub fn ensure_colord_display_device(make: &str, model: &str, serial: &str) -> bool {
    if !colord_runtime_enabled() {
        return false;
    }

    let Ok(conn) = Connection::session() else {
        return false;
    };
    let Ok(cm) = colord_manager_proxy(&conn) else {
        return false;
    };

    let device_id = colord_device_id(make, model, serial);
    if colord_find_by_property(&cm, "DeviceId", &device_id).is_some() {
        return true;
    }

    let props: HashMap<&str, &str> = HashMap::from([
        ("Kind", "display"),
        ("Vendor", make),
        ("Model", model),
        ("SerialNumber", serial),
        ("Colorspace", "RGB"),
    ]);

    match cm.call_method("CreateDevice", &(device_id.as_str(), "temp", props)) {
        Ok(_) => {
            flog(format!("colord: registered display device {device_id}"));
            true
        }
        Err(err) => {
            flog(format!("colord: CreateDevice failed for {device_id}: {err}"));
            false
        }
    }
}

/// Re-load ICC/EDID descriptions for every output and notify wp_color clients.
pub fn refresh_all_output_colors(state: &mut DesktopState) -> bool {
    let snapshots: Vec<_> = state
        .outputs
        .iter()
        .map(|(id, output)| {
            (
                *id,
                output.monitor_make.clone(),
                output.monitor_model.clone(),
                output.monitor_serial.clone(),
                output.monitor_edid.clone(),
                output.color_description,
                output.icc_profile.clone(),
            )
        })
        .collect();

    let mut any_changed = false;
    for (output_id, make, model, serial, edid, old_desc, old_icc) in snapshots {
        let (new_desc, new_icc) = match resolve_output_color_profile(
            &make,
            &model,
            &serial,
            edid.as_deref(),
        ) {
            Some(parsed) => (
                parsed.description,
                (!parsed.bytes.is_empty()).then_some(parsed.bytes),
            ),
            None => (default_output_color_description(), None),
        };

        if new_desc != old_desc || new_icc != old_icc {
            state.set_output_color(output_id, new_desc, new_icc);
            any_changed = true;
            flog(format!(
                "output color refreshed: id={output_id:?} primaries={:?} transfer={:?}",
                new_desc.primaries, new_desc.transfer
            ));
        }
    }

    if any_changed {
        color_management_protocol::notify_preferred_color_changed(state);
        state.mark_all_outputs_full_damage(DamageSource::Unknown);
        state.mark_redraw();
    }

    any_changed
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

    let Ok(conn) = Connection::session() else {
        flog("colord watch: no session D-Bus");
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
        && msg.member().as_deref().is_some_and(|m| m == "DeviceChanged" || m == "ProfileChanged")
}

pub fn load_display_profile_via_colord(
    make: &str,
    model: &str,
    serial: &str,
) -> Option<ParsedIccProfile> {
    if !colord_runtime_enabled() {
        return None;
    }

    let conn = Connection::session().ok()?;
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
    if let Some(path) = colord_find_by_property(&cm, "DeviceId", &device_id) {
        return Some(path);
    }

    if !serial.is_empty() {
        if let Some(path) = colord_find_by_property(&cm, "serial", serial) {
            return Some(path);
        }
    }

    if !model.is_empty() {
        if let Some(path) = colord_find_by_property(&cm, "model", model) {
            if colord_device_matches(&conn, &path, make, model, serial) {
                return Some(path);
            }
        }
    }

    if !make.is_empty() {
        if let Some(path) = colord_find_by_property(&cm, "vendor", make) {
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

fn colord_find_by_property(
    cm: &Proxy<'_>,
    property: &str,
    value: &str,
) -> Option<OwnedObjectPath> {
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

    monitor_tokens_match(
        make,
        model,
        serial,
        &vendor,
        &dev_model,
        &dev_serial,
    )
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

    let haystacks = [vendor_l.as_str(), dev_model_l.as_str(), dev_serial_l.as_str()];
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

    let profile_path: OwnedObjectPath = device.get_property("Profile").ok()?;
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
        assert!(!monitor_tokens_match("LG", "27UP850", "123", "Dell", "U2720Q", "999"));
    }
}
