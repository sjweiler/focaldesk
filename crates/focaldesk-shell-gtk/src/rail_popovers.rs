//! Rail-owned GTK popovers. Service calls run off the GTK thread; closing a
//! popup drops its view, so late replies cannot update a subsequent popup.
use focaldesk_ipc::{
    send_notification_request, send_update_request, DesktopAction, NotificationIpcRequest as NReq,
    NotificationIpcResponse as NResp, ShellPanel, UpdateIpcRequest as UReq,
    UpdateIpcResponse as UResp,
};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    process::Command,
    rc::Rc,
    time::Duration,
};

thread_local! {
    static OPEN: RefCell<Option<glib::WeakRef<gtk::Popover>>> = const { RefCell::new(None) };
}

#[derive(Clone)]
struct View {
    content: glib::WeakRef<gtk::Box>,
    status: glib::WeakRef<gtk::Label>,
    popup: glib::WeakRef<gtk::Popover>,
    generation: Rc<Cell<u64>>,
}
impl View {
    fn message(&self, text: &str) {
        if let Some(label) = self.status.upgrade() {
            label.set_text(text);
        }
    }
    fn clear(&self) -> Option<gtk::Box> {
        let body = self.content.upgrade()?;
        while let Some(child) = body.first_child() {
            body.remove(&child);
        }
        Some(body)
    }
}

fn background<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    done: impl FnOnce(Result<T, String>) + 'static,
) {
    glib::MainContext::default().spawn_local(async move {
        let result = gio::spawn_blocking(work)
            .await
            .unwrap_or_else(|_| Err("The operation could not finish. Try again.".into()));
        done(result);
    });
}

fn load<T: Send + 'static>(
    view: &View,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
    render: impl FnOnce(&View, T) + 'static,
) {
    let generation = view.generation.get().wrapping_add(1);
    view.generation.set(generation);
    view.message("Loading…");
    let view = view.clone();
    background(work, move |result| {
        if view.generation.get() != generation
            || !view.popup.upgrade().is_some_and(|p| p.is_visible())
        {
            return;
        }
        match result {
            Ok(value) => {
                view.message("");
                render(&view, value);
            }
            Err(error) => view.message(&error),
        }
    });
}

fn text(value: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(value));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.set_max_width_chars(44);
    label.set_selectable(true);
    label
}

fn action(
    body: &gtk::Box,
    view: &View,
    label: &str,
    work: impl Fn() -> Result<(), String> + Clone + Send + 'static,
    refresh: Option<fn(&View)>,
) -> gtk::Button {
    let button = gtk::Button::with_label(label);
    let view = view.clone();
    button.connect_clicked(move |button| {
        button.set_sensitive(false);
        view.message("Working…");
        let weak = button.downgrade();
        let view = view.clone();
        background(work.clone(), move |result| {
            if let Some(button) = weak.upgrade() {
                button.set_sensitive(true);
            }
            match result {
                Ok(()) => {
                    view.message("Done");
                    if let Some(refresh) = refresh {
                        refresh(&view);
                    }
                }
                Err(error) => view.message(&error),
            }
        });
    });
    body.append(&button);
    button
}

fn command(program: &str, args: &[&str]) -> Result<String, String> {
    let output = Command::new("timeout")
        .args(["20s", program])
        .args(args)
        .output()
        .map_err(|e| format!("Could not run {program}: {e}"))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if detail.is_empty() {
            format!("{program} did not complete. Check the service and try again.")
        } else {
            detail.chars().take(400).collect()
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn settings(body: &gtk::Box, view: &View, page: &str) {
    let button = gtk::Button::with_label("Open Settings…");
    let page = page.to_owned();
    let popup = view.popup.clone();
    button.connect_clicked(move |_| {
        if let Some(popup) = popup.upgrade() {
            popup.popdown();
        }
        super::send_action(DesktopAction::OpenSettingsPanel {
            panel: page.clone(),
        });
    });
    body.append(&button);
}

pub(super) fn attach(button: &gtk::Button, connector: &str, panel: ShellPanel) {
    let connector = connector.to_owned();
    button.connect_clicked(move |button| show(button, None, &connector, panel));
}

pub(super) fn show(
    anchor: &impl IsA<gtk::Widget>,
    rect: Option<gdk::Rectangle>,
    _connector: &str,
    panel: ShellPanel,
) {
    if matches!(panel, ShellPanel::Display | ShellPanel::Settings) {
        super::send_action(DesktopAction::OpenSettingsPanel {
            panel: if matches!(panel, ShellPanel::Display) {
                "displays"
            } else {
                "privacy"
            }
            .into(),
        });
        return;
    }
    // A single open popup across all rails, including rails on other monitors.
    OPEN.with(|open| {
        if let Some(previous) = open.borrow_mut().take().and_then(|p| p.upgrade()) {
            previous.popdown();
        }
    });
    let popup = gtk::Popover::new();
    popup.add_css_class("rail-popover");
    popup.set_parent(anchor);
    popup.set_position(gtk::PositionType::Left);
    popup.set_autohide(true);
    popup.set_has_arrow(true);
    if let Some(rect) = rect {
        popup.set_pointing_to(Some(&rect));
    }
    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_width_request(320);
    body.set_margin_top(14);
    body.set_margin_bottom(14);
    body.set_margin_start(14);
    body.set_margin_end(14);
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let title = text(match panel {
        ShellPanel::Network => "Network",
        ShellPanel::Bluetooth => "Bluetooth",
        ShellPanel::Audio => "Audio",
        ShellPanel::Calendar => "Calendar",
        ShellPanel::NotificationHistory => "Notifications",
        ShellPanel::Updates => "System updates",
        ShellPanel::Power => "Power",
        _ => "Settings",
    });
    title.add_css_class("heading");
    title.set_hexpand(true);
    header.append(&title);
    let close = gtk::Button::from_icon_name("window-close-symbolic");
    close.set_tooltip_text(Some("Close"));
    let weak_popup = popup.downgrade();
    close.connect_clicked(move |_| {
        if let Some(p) = weak_popup.upgrade() {
            p.popdown();
        }
    });
    header.append(&close);
    body.append(&header);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .max_content_height(440)
        .propagate_natural_height(true)
        .child(&content)
        .build();
    body.append(&scroll);
    let status = text("");
    status.add_css_class("rail-popover-status");
    body.append(&status);
    let view = View {
        content: content.downgrade(),
        status: status.downgrade(),
        popup: popup.downgrade(),
        generation: Rc::new(Cell::new(0)),
    };
    popup.set_child(Some(&body));
    let weak_anchor = anchor.downgrade();
    let host_popup = popup.downgrade();
    anchor.connect_unrealize(move |_| {
        if let Some(p) = host_popup.upgrade() {
            p.popdown();
        }
    });
    popup.connect_closed(move |popup| {
        popup.unparent();
        if let Some(anchor) = weak_anchor.upgrade() {
            anchor.grab_focus();
        }
    });
    // GTK handles modal outside-click dismissal; keep Escape explicit for both hosts.
    let keys = gtk::EventControllerKey::new();
    let weak_popup = popup.downgrade();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            if let Some(p) = weak_popup.upgrade() {
                p.popdown();
            }
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    popup.add_controller(keys);
    OPEN.with(|open| *open.borrow_mut() = Some(popup.downgrade()));
    popup.popup();
    match panel {
        ShellPanel::Network => network(&view),
        ShellPanel::Bluetooth => bluetooth(&view),
        ShellPanel::Audio => audio(&view),
        ShellPanel::Calendar => calendar(&view),
        ShellPanel::NotificationHistory => notifications(&view),
        ShellPanel::Updates => updates(&view),
        ShellPanel::Power => power(&view),
        _ => settings(&content, &view, "appearance"),
    }
}

// nmcli terse output escapes delimiters and backslashes inside SSIDs.
fn fields(line: &str) -> Vec<String> {
    let mut values = vec![String::new()];
    let mut escaped = false;
    for ch in line.chars() {
        if escaped {
            values.last_mut().unwrap().push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == ':' {
            values.push(String::new());
        } else {
            values.last_mut().unwrap().push(ch);
        }
    }
    if escaped {
        values.last_mut().unwrap().push('\\');
    }
    values
}

fn network(view: &View) {
    load(
        view,
        || {
            let enabled = command("nmcli", &["radio", "wifi"])?.trim() == "enabled";
            let devices = command(
                "nmcli",
                &[
                    "-t",
                    "-f",
                    "DEVICE,TYPE,STATE,CONNECTION",
                    "device",
                    "status",
                ],
            )?;
            let networks = if enabled {
                command(
                    "nmcli",
                    &[
                        "-t",
                        "-f",
                        "IN-USE,SSID,SIGNAL,SECURITY",
                        "device",
                        "wifi",
                        "list",
                        "--rescan",
                        "no",
                    ],
                )?
            } else {
                String::new()
            };
            let saved = command(
                "nmcli",
                &["-t", "-f", "NAME,UUID,TYPE", "connection", "show"],
            )?;
            Ok((enabled, devices, networks, saved))
        },
        |view, (enabled, devices, networks, saved)| {
            let Some(body) = view.clear() else {
                return;
            };
            for line in devices.lines() {
                let f = fields(line);
                if f.len() == 4 && f[2] == "connected" {
                    body.append(&text(&format!("{} · {}", f[0], f[3])));
                }
            }
            action(
                &body,
                view,
                if enabled {
                    "Turn Wi-Fi off"
                } else {
                    "Turn Wi-Fi on"
                },
                move || {
                    command(
                        "nmcli",
                        &["radio", "wifi", if enabled { "off" } else { "on" }],
                    )
                    .map(|_| ())
                },
                Some(network),
            );
            let saved: Vec<_> = saved
                .lines()
                .map(fields)
                .filter(|f| f.len() == 3 && f[2] == "802-11-wireless")
                .collect();
            let mut seen = std::collections::HashSet::new();
            for f in networks
                .lines()
                .map(fields)
                .filter(|f| f.len() == 4 && !f[1].is_empty())
            {
                if !seen.insert(f[1].clone()) {
                    continue;
                }
                body.append(&text(&format!(
                    "{} · {}%{}",
                    f[1],
                    f[2],
                    if f[0] == "*" { " · Connected" } else { "" }
                )));
                if f[0] != "*" {
                    if let Some(profile) = saved.iter().find(|p| p[0] == f[1]) {
                        let uuid = profile[1].clone();
                        action(
                            &body,
                            view,
                            "Connect",
                            move || {
                                command(
                                    "nmcli",
                                    &["--wait", "15", "connection", "up", "uuid", &uuid],
                                )
                                .map(|_| ())
                            },
                            Some(network),
                        );
                    } else {
                        body.append(&text("Use Network Settings to join a new network."));
                    }
                }
            }
            if enabled && seen.is_empty() {
                body.append(&text("No Wi-Fi networks found."));
            }
            action(
                &body,
                view,
                "Scan again",
                || command("nmcli", &["device", "wifi", "rescan"]).map(|_| ()),
                Some(network),
            );
            settings(&body, view, "network");
        },
    );
}

fn bluetooth(view: &View) {
    load(
        view,
        || {
            command("bluetoothctl", &["show"])?;
            Ok(super::bluetooth_state())
        },
        |view, state| {
            let Some(body) = view.clear() else {
                return;
            };
            body.append(&text(&super::bluetooth_summary(&state)));
            let powered = state.powered;
            action(
                &body,
                view,
                if powered {
                    "Turn Bluetooth off"
                } else {
                    "Turn Bluetooth on"
                },
                move || {
                    command(
                        "bluetoothctl",
                        &["power", if powered { "off" } else { "on" }],
                    )
                    .map(|_| ())
                },
                Some(bluetooth),
            );
            for device in state.devices {
                body.append(&text(&device.name));
                let address = device.address;
                action(
                    &body,
                    view,
                    if device.connected {
                        "Disconnect"
                    } else {
                        "Connect"
                    },
                    move || {
                        command(
                            "bluetoothctl",
                            &[
                                if device.connected {
                                    "disconnect"
                                } else {
                                    "connect"
                                },
                                &address,
                            ],
                        )
                        .map(|_| ())
                    },
                    Some(bluetooth),
                );
            }
            action(&body, view, "Refresh devices", || Ok(()), Some(bluetooth));
            settings(&body, view, "bluetooth");
        },
    );
}

#[allow(deprecated)] // ComboBoxText supports the GTK 4.8 baseline.
fn audio(view: &View) {
    load(
        view,
        || {
            let volume = command("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"])?;
            Ok((
                volume,
                command("wpctl", &["get-volume", "@DEFAULT_AUDIO_SOURCE@"]).ok(),
                super::pipewire_nodes("Sinks"),
                super::pipewire_nodes("Sources"),
            ))
        },
        |view, (volume, microphone_volume, sinks, sources)| {
            let Some(body) = view.clear() else {
                return;
            };
            let percent = volume
                .split_whitespace()
                .find_map(|s| s.parse::<f64>().ok())
                .unwrap_or(0.0)
                * 100.0;
            body.append(&text("Output volume"));
            let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
            scale.set_value(percent.clamp(0.0, 100.0));
            scale.set_draw_value(true);
            scale.set_digits(0);
            scale.update_property(&[gtk::accessible::Property::Label("Output volume")]);
            let pending = Rc::new(RefCell::new(None::<glib::SourceId>));
            let status = view.status.clone();
            scale.connect_value_changed(move |scale| {
                if let Some(source) = pending.borrow_mut().take() {
                    source.remove();
                }
                let weak_scale = scale.downgrade();
                let status = status.clone();
                let pending_done = pending.clone();
                *pending.borrow_mut() = Some(glib::timeout_add_local_once(
                    Duration::from_millis(150),
                    move || {
                        pending_done.borrow_mut().take();
                        let Some(scale) = weak_scale.upgrade() else {
                            return;
                        };
                        let value = scale.value();
                        background(
                            move || super::set_pipewire_volume(value),
                            move |result| {
                                if let (Err(error), Some(label)) = (result, status.upgrade()) {
                                    label.set_text(&error);
                                }
                            },
                        );
                    },
                ));
            });
            body.append(&scale);
            action(
                &body,
                view,
                if volume.contains("MUTED") {
                    "Unmute output"
                } else {
                    "Mute output"
                },
                || command("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]).map(|_| ()),
                Some(audio),
            );
            for (label, nodes) in [("Output device", sinks), ("Microphone", sources)] {
                body.append(&text(label));
                let picker = gtk::ComboBoxText::new();
                for node in &nodes {
                    picker.append(Some(&node.id), &node.name);
                }
                if let Some(node) = nodes.iter().find(|n| n.active) {
                    picker.set_active_id(Some(&node.id));
                }
                picker.set_sensitive(!nodes.is_empty());
                let view = view.clone();
                picker.connect_changed(move |picker| {
                    if let Some(id) = picker.active_id() {
                        let id = id.to_string();
                        let view = view.clone();
                        background(
                            move || command("wpctl", &["set-default", &id]).map(|_| ()),
                            move |result| match result {
                                Ok(()) => audio(&view),
                                Err(e) => view.message(&e),
                            },
                        );
                    }
                });
                body.append(&picker);
            }
            let microphone_mute = action(
                &body,
                view,
                if microphone_volume
                    .as_ref()
                    .is_some_and(|value| value.contains("MUTED"))
                {
                    "Unmute microphone"
                } else {
                    "Mute microphone"
                },
                || command("wpctl", &["set-mute", "@DEFAULT_AUDIO_SOURCE@", "toggle"]).map(|_| ()),
                Some(audio),
            );
            microphone_mute.set_sensitive(microphone_volume.is_some());
            settings(&body, view, "sound");
        },
    );
}

fn calendar(view: &View) {
    let Some(body) = view.clear() else {
        return;
    };
    body.append(&text(
        &chrono::Local::now().format("%A, %B %-d, %Y").to_string(),
    ));
    body.append(&gtk::Calendar::new());
    let button = gtk::Button::with_label("Open Calendar…");
    let popup = view.popup.clone();
    button.connect_clicked(move |_| {
        if let Some(p) = popup.upgrade() {
            p.popdown();
        }
        super::send_action(DesktopAction::LaunchApp {
            app: "focaldesk-calendar".into(),
        });
    });
    body.append(&button);
}

fn notification_action(request: NReq) -> Result<(), String> {
    match send_notification_request(&request)? {
        NResp::Ok => Ok(()),
        NResp::Error { message } => Err(message),
        _ => Err("Unexpected notification response".into()),
    }
}
fn notifications(view: &View) {
    load(
        view,
        || {
            let history = send_notification_request(&NReq::GetHistory)?;
            let state = send_notification_request(&NReq::GetState)?;
            Ok((history, state))
        },
        |view, (history, state)| {
            let Some(body) = view.clear() else {
                return;
            };
            let NResp::History {
                notifications: items,
            } = history
            else {
                view.message(&match history {
                    NResp::Error { message } => message,
                    _ => "Could not read notifications".into(),
                });
                return;
            };
            let NResp::State { do_not_disturb } = state else {
                view.message("Could not read Do Not Disturb state");
                return;
            };
            action(
                &body,
                view,
                if do_not_disturb {
                    "Turn Do Not Disturb off"
                } else {
                    "Turn Do Not Disturb on"
                },
                move || {
                    notification_action(NReq::SetDoNotDisturb {
                        enabled: !do_not_disturb,
                    })
                },
                Some(notifications),
            );
            if items.is_empty() {
                body.append(&text("No notifications."));
            }
            for item in items.into_iter().rev().take(50) {
                let card = gtk::Box::new(gtk::Orientation::Vertical, 5);
                card.add_css_class("rail-notification");
                card.append(&text(&item.title));
                card.append(&text(&item.body));
                let id = item.id;
                for a in item.actions {
                    action(
                        &card,
                        view,
                        &a.label,
                        move || {
                            notification_action(NReq::InvokeAction {
                                id,
                                action_key: a.key.clone(),
                            })
                        },
                        Some(notifications),
                    );
                }
                body.append(&card);
            }
            action(
                &body,
                view,
                "Mark all read",
                || notification_action(NReq::MarkAllRead),
                Some(notifications),
            );
            action(
                &body,
                view,
                "Clear history",
                || notification_action(NReq::ClearHistory),
                Some(notifications),
            );
            action(&body, view, "Refresh", || Ok(()), Some(notifications));
        },
    );
}

fn update_action(request: UReq) -> Result<(), String> {
    match send_update_request(&request)? {
        UResp::Ok | UResp::Accepted => Ok(()),
        UResp::Error { message } => Err(message),
        _ => Err("Unexpected update response".into()),
    }
}
fn updates(view: &View) {
    load(
        view,
        || match send_update_request(&UReq::GetState)? {
            UResp::State { snapshot } => Ok(snapshot),
            UResp::Error { message } => Err(message),
            _ => Err("Could not read updates".into()),
        },
        |view, snapshot| {
            let Some(body) = view.clear() else {
                return;
            };
            body.append(&text(&format!(
                "{} updates available",
                snapshot.packages.len()
            )));
            if let Some(progress) = snapshot.progress {
                body.append(&text(&progress));
            }
            if let Some(error) = snapshot.last_error {
                view.message(&error);
            }
            for package in &snapshot.packages {
                body.append(&text(&package.display_title()));
            }
            let busy = snapshot.checking || snapshot.installing;
            if busy {
                body.append(&text("Update operation in progress…"));
                let view = view.clone();
                let generation = view.generation.get();
                glib::timeout_add_local_once(Duration::from_secs(2), move || {
                    if view.generation.get() == generation
                        && view.popup.upgrade().is_some_and(|p| p.is_visible())
                    {
                        updates(&view);
                    }
                });
            }
            let check = action(
                &body,
                view,
                "Check for updates",
                || {
                    update_action(UReq::Refresh {
                        refresh_metadata: true,
                    })
                },
                Some(updates),
            );
            check.set_sensitive(!busy);
            if !snapshot.packages.is_empty() {
                let install = action(
                    &body,
                    view,
                    "Install all updates",
                    || update_action(UReq::InstallAll),
                    Some(updates),
                );
                install.set_sensitive(!busy);
            }
            action(&body, view, "Refresh status", || Ok(()), Some(updates));
        },
    );
}

fn power(view: &View) {
    let Some(body) = view.clear() else {
        return;
    };
    body.append(&text("Power and session"));
    action(
        &body,
        view,
        "Lock",
        || command("loginctl", &["lock-session"]).map(|_| ()),
        None,
    );
    action(
        &body,
        view,
        "Suspend",
        || super::send_companion_power_request(super::PowerIpcRequest::Suspend),
        None,
    );
    confirm_power_action(&body, view, "Restart", || {
        super::send_companion_power_request(super::PowerIpcRequest::Reboot)
    });
    confirm_power_action(&body, view, "Shut down", || {
        super::send_companion_power_request(super::PowerIpcRequest::PowerOff)
    });
    confirm_power_action(
        &body,
        view,
        "Log out",
        || match super::send_desktop_request(&super::IpcRequest::ExecuteDesktopAction {
            action: DesktopAction::Logout,
        })? {
            super::IpcResponse::Ok => Ok(()),
            super::IpcResponse::Error { message } => Err(message),
            _ => Err("Unexpected session response".into()),
        },
    );
    settings(&body, view, "power");
}

fn confirm_power_action(
    body: &gtk::Box,
    view: &View,
    label: &'static str,
    work: impl Fn() -> Result<(), String> + Clone + Send + 'static,
) {
    let button = gtk::Button::with_label(label);
    let view = view.clone();
    button.connect_clicked(move |_| {
        let Some(body) = view.clear() else {
            return;
        };
        body.append(&text(&format!("{label}? Save your work first.")));
        action(
            &body,
            &view,
            &format!("Confirm {label}"),
            work.clone(),
            None,
        );
        let cancel = gtk::Button::with_label("Cancel");
        let cancel_view = view.clone();
        cancel.connect_clicked(move |_| power(&cancel_view));
        body.append(&cancel);
    });
    body.append(&button);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nmcli_fields_preserve_escaped_ssids_and_empty_security() {
        assert_eq!(
            fields(r"*:Office\:West:90:WPA2"),
            ["*", "Office:West", "90", "WPA2"]
        );
        assert_eq!(fields(r":Guest\\WiFi:35:"), ["", r"Guest\WiFi", "35", ""]);
    }
    #[test]
    #[ignore = "requires GTK display; run under gtk4-broadwayd"]
    fn native_popover_lifecycle() {
        gtk::init().expect("GTK display");
        gtk::Settings::default()
            .unwrap()
            .set_gtk_enable_animations(false);
        let window = gtk::Window::new();
        let button = gtk::Button::with_label("Calendar");
        window.set_child(Some(&button));
        window.present();
        let drain = || {
            while glib::MainContext::default().pending() {
                glib::MainContext::default().iteration(false);
            }
        };
        drain();
        show(&button, None, "DP-3", ShellPanel::Calendar);
        drain();
        let first = OPEN.with(|p| p.borrow().as_ref().unwrap().upgrade().unwrap());
        assert!(first.is_visible());
        assert_eq!(first.parent().as_ref(), Some(button.upcast_ref()));
        assert_eq!(first.position(), gtk::PositionType::Left);
        assert!(first.is_autohide());
        show(&button, None, "DP-4", ShellPanel::Calendar);
        drain();
        assert!(
            first.parent().is_none(),
            "switching rails dismisses and unparents the old popup"
        );
        let second = OPEN.with(|p| p.borrow().as_ref().unwrap().upgrade().unwrap());
        second.popdown();
        drain();
        assert!(second.parent().is_none());
        let drawing = gtk::DrawingArea::new();
        drawing.set_content_width(64);
        drawing.set_content_height(300);
        drawing.set_focusable(true);
        window.set_child(Some(&drawing));
        drain();
        show(
            &drawing,
            Some(gdk::Rectangle::new(8, 20, 44, 44)),
            "DP-4",
            ShellPanel::Calendar,
        );
        drain();
        let vui_popup = OPEN.with(|p| p.borrow().as_ref().unwrap().upgrade().unwrap());
        assert!(vui_popup.is_visible());
        assert_eq!(
            vui_popup.pointing_to(),
            (true, gdk::Rectangle::new(8, 20, 44, 44))
        );
        let key_controller = (0..vui_popup.observe_controllers().n_items())
            .filter_map(|i| vui_popup.observe_controllers().item(i))
            .find_map(|controller| controller.downcast::<gtk::EventControllerKey>().ok())
            .expect("Escape controller");
        let handled = key_controller.emit_by_name::<bool>(
            "key-pressed",
            &[&gdk::Key::Escape, &0_u32, &gdk::ModifierType::empty()],
        );
        assert!(handled);
        drain();
        assert!(vui_popup.parent().is_none(), "Escape dismisses the popup");
        let original_path = std::env::var_os("PATH");
        std::env::set_var("PATH", "/nonexistent");
        show(&drawing, None, "DP-4", ShellPanel::Audio);
        let failed_popup = OPEN.with(|p| p.borrow().as_ref().unwrap().upgrade().unwrap());
        fn labels(widget: &gtk::Widget) -> Vec<String> {
            let mut values = widget
                .downcast_ref::<gtk::Label>()
                .map(|l| vec![l.text().to_string()])
                .unwrap_or_default();
            let mut child = widget.first_child();
            while let Some(w) = child {
                values.extend(labels(&w));
                child = w.next_sibling();
            }
            values
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let mut shown_error = false;
        while std::time::Instant::now() < deadline {
            drain();
            shown_error = labels(failed_popup.upcast_ref())
                .iter()
                .any(|s| s.contains("Could not run wpctl"));
            if shown_error {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        if let Some(path) = original_path {
            std::env::set_var("PATH", path);
        } else {
            std::env::remove_var("PATH");
        }
        assert!(shown_error, "service failures must be visible in the popup");
        failed_popup.popdown();
        drain();
        window.close();
        drain();
    }
}
