use adw::prelude::*;
use focaldesk_config::{load_config, save_config, FocalDeskConfig};
use focaldesk_ipc::{
    send_desktop_config, send_desktop_request, send_desktop_set, watch_desktop_keys, IpcRequest,
    IpcResponse,
};
use focaldesk_logging::flog_info;
use focaldesk_settings_core::OutputConfig;

use gtk::cairo;
use gtk::glib;
use serde_json::json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

const SCALE_OPTIONS: &[(&str, f64)] = &[
    ("100 %", 1.0),
    ("125 %", 1.25),
    ("133 %", 1.3333334),
    ("150 %", 1.5),
    ("166 %", 1.6666667),
    ("200 %", 2.0),
    ("250 %", 2.5),
    ("266 %", 2.6666667),
];

const THEME_OPTIONS: &[&str] = &["Eagle", "Moonbase", "Classic"];
const ORIENTATION_OPTIONS: &[&str] = &[
    "Landscape",
    "Portrait Right",
    "Landscape Flipped",
    "Portrait Left",
];
const OUTPUT_DEVICE_OPTIONS: &[&str] = &[
    "Default Output",
    "S/PDIF Output - USB Audio",
    "HDMI / DisplayPort",
];
const OUTPUT_CONFIGURATION_OPTIONS: &[&str] = &["HiFi 2.0 channels", "Stereo", "Mono"];
const ALERT_SOUND_OPTIONS: &[&str] = &["Default", "Click", "Chime", "None"];

#[derive(Debug)]
struct ConfigEvent {
    key: String,
    value: serde_json::Value,
}

#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
struct DisplayConfig {
    name: String,
    enabled: bool,

    mode_width: i32,
    mode_height: i32,
    refresh_mhz: i32,

    scale: f64,

    logical_x: i32,
    logical_y: i32,

    physical_width_mm: Option<i32>,
    physical_height_mm: Option<i32>,

    primary: bool,
    transform: String,

    #[serde(default)]
    hdr_supported: bool,
    #[serde(default)]
    hdr_enabled: bool,
}

fn save_displays(displays: &[DisplayConfig]) {
    let path = displays_path();

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(text) = serde_json::to_string_pretty(displays) {
        let _ = std::fs::write(path, text);
    }

    let outputs = displays
        .iter()
        .map(|display| OutputConfig {
            connector: display.name.clone(),
            enabled: display.enabled,
            x: display.logical_x,
            y: display.logical_y,
            width: display.mode_width,
            height: display.mode_height,
            refresh_mhz: display.refresh_mhz,
            scale: display.scale as f32,
            primary: display.primary,
        })
        .collect();

    match send_desktop_request(&IpcRequest::SetDisplays { outputs }) {
        Ok(IpcResponse::Ok) => {}
        Ok(IpcResponse::Error { message }) => {
            flog_info!("display IPC update rejected: {message}");
        }
        Ok(other) => {
            flog_info!("unexpected display IPC response: {other:?}");
        }
        Err(err) => {
            flog_info!("display IPC unavailable; saved display config directly: {err}");
        }
    }
}

fn display_preview_rect(
    d: &DisplayConfig,
    zoom: f64,
    offset_x: f64,
    offset_y: f64,
) -> (f64, f64, f64, f64) {
    let (width, height) = if matches!(d.transform.as_str(), "Rotate90" | "Rotate270") {
        (d.mode_height, d.mode_width)
    } else {
        (d.mode_width, d.mode_height)
    };
    let logical_w = width as f64 / d.scale.max(1.0);
    let logical_h = height as f64 / d.scale.max(1.0);

    (
        d.logical_x as f64 * zoom + offset_x,
        d.logical_y as f64 * zoom + offset_y,
        logical_w * zoom,
        logical_h * zoom,
    )
}

fn monitor_arrangement_area(displays: Rc<RefCell<Vec<DisplayConfig>>>) -> gtk::DrawingArea {
    let area = gtk::DrawingArea::new();
    area.set_content_width(700);
    area.set_content_height(260);
    area.set_hexpand(true);

    let selected = Rc::new(RefCell::new(None::<usize>));
    let drag_start = Rc::new(RefCell::new((0, 0)));

    let zoom = 0.12;
    let offset_x = 40.0;
    let offset_y = 40.0;

    {
        let displays = displays.clone();

        area.set_draw_func(move |_, cr, _width, _height| {
            cr.set_source_rgb(0.08, 0.09, 0.11);
            let _ = cr.paint();

            let displays = displays.borrow();

            for d in displays.iter() {
                let (x, y, w, h) = display_preview_rect(d, zoom, offset_x, offset_y);

                cr.set_source_rgb(0.18, 0.22, 0.28);
                rounded_rect(cr, x, y, w, h, 14.0);
                let _ = cr.fill_preserve();

                if d.primary {
                    cr.set_source_rgb(0.75, 0.85, 1.0);
                    cr.set_line_width(3.0);
                } else {
                    cr.set_source_rgb(0.45, 0.60, 0.75);
                    cr.set_line_width(2.0);
                }

                let _ = cr.stroke();

                cr.set_source_rgb(0.95, 0.97, 1.0);
                cr.move_to(x + 14.0, y + 28.0);
                let _ = cr.show_text(&d.name);

                cr.move_to(x + 14.0, y + 50.0);
                let _ = cr.show_text(&format!(
                    "{}×{} @ {}Hz",
                    d.mode_width, d.mode_height, d.refresh_mhz
                ));
            }
        });
    }

    let drag = gtk::GestureDrag::new();

    {
        let displays = displays.clone();
        let selected = selected.clone();
        let drag_start = drag_start.clone();

        drag.connect_drag_begin(move |_, x, y| {
            let displays_ref = displays.borrow();

            let hit = displays_ref.iter().position(|d| {
                let (rx, ry, rw, rh) = display_preview_rect(d, zoom, offset_x, offset_y);
                x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
            });

            *selected.borrow_mut() = hit;

            if let Some(i) = hit {
                let d = &displays_ref[i];
                *drag_start.borrow_mut() = (d.logical_x, d.logical_y);
            }
        });
    }

    {
        let displays = displays.clone();
        let selected = selected.clone();
        let drag_start = drag_start.clone();
        let area_clone = area.clone();

        drag.connect_drag_update(move |_, dx, dy| {
            if let Some(i) = *selected.borrow() {
                let (start_x, start_y) = *drag_start.borrow();

                let mut displays = displays.borrow_mut();
                displays[i].logical_x = start_x + (dx / zoom).round() as i32;
                displays[i].logical_y = start_y + (dy / zoom).round() as i32;

                area_clone.queue_draw();
            }
        });
    }

    {
        let displays = displays.clone();
        let selected = selected.clone();

        drag.connect_drag_end(move |_, _, _| {
            *selected.borrow_mut() = None;
            save_displays(&displays.borrow());
        });
    }

    area.add_controller(drag);
    area
}

fn rounded_rect(cr: &cairo::Context, x: f64, y: f64, w: f64, h: f64, r: f64) {
    cr.new_sub_path();
    cr.arc(x + w - r, y + r, r, -90f64.to_radians(), 0f64.to_radians());
    cr.arc(
        x + w - r,
        y + h - r,
        r,
        0f64.to_radians(),
        90f64.to_radians(),
    );
    cr.arc(x + r, y + h - r, r, 90f64.to_radians(), 180f64.to_radians());
    cr.arc(x + r, y + r, r, 180f64.to_radians(), 270f64.to_radians());
    cr.close_path();
}

fn displays_path() -> std::path::PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap()).join(".config")
        })
        .join("focaldesk")
        .join("displays.json")
}

fn load_displays() -> Vec<DisplayConfig> {
    let path = displays_path();

    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => vec![],
    }
}

fn display_summary(d: &DisplayConfig) -> String {
    format!(
        "{}x{} @ {} Hz  |  {}  |  Scale {:.2}{}{}",
        d.mode_width,
        d.mode_height,
        d.refresh_mhz / 1000,
        transform_label(&d.transform),
        d.scale,
        if d.primary { "  |  Primary" } else { "" },
        if d.enabled { "" } else { "  |  Disabled" }
    )
}

fn transform_label(transform: &str) -> &'static str {
    match transform {
        "Rotate90" => "Portrait Right",
        "Rotate180" => "Landscape Flipped",
        "Rotate270" => "Portrait Left",
        _ => "Landscape",
    }
}

fn transform_index(transform: &str) -> u32 {
    match transform {
        "Rotate90" => 1,
        "Rotate180" => 2,
        "Rotate270" => 3,
        _ => 0,
    }
}

fn transform_from_index(index: u32) -> &'static str {
    match index {
        1 => "Rotate90",
        2 => "Rotate180",
        3 => "Rotate270",
        _ => "Normal",
    }
}

fn resolution_options(current_width: i32, current_height: i32) -> Vec<(i32, i32)> {
    let mut options = vec![
        (1280, 720),
        (1366, 768),
        (1600, 900),
        (1920, 1080),
        (2560, 1440),
        (3440, 1440),
        (3840, 2160),
    ];

    if !options.contains(&(current_width, current_height)) {
        options.push((current_width, current_height));
    }

    options.sort_unstable();
    options.dedup();
    options
}

fn dropdown_from_strings(labels: &[&str], selected: u32) -> gtk::DropDown {
    let dropdown = gtk::DropDown::from_strings(labels);
    dropdown.set_selected(selected);
    dropdown
}

fn dim_label(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("dim-label");
    label
}

fn suffix_chevron() -> gtk::Image {
    gtk::Image::from_icon_name("go-next-symbolic")
}

fn save_display_change(
    displays: &Rc<RefCell<Vec<DisplayConfig>>>,
    area: &gtk::DrawingArea,
    row: &adw::ExpanderRow,
    index: usize,
) {
    let displays_ref = displays.borrow();
    save_displays(&displays_ref);
    area.queue_draw();

    if let Some(display) = displays_ref.get(index) {
        row.set_subtitle(&display_summary(display));
    }
}

fn connected_display_row(
    index: usize,
    displays: Rc<RefCell<Vec<DisplayConfig>>>,
    area: gtk::DrawingArea,
) -> adw::ExpanderRow {
    let display = displays.borrow()[index].clone();
    let row = adw::ExpanderRow::new();
    row.set_title(&display.name);
    row.set_subtitle(&display_summary(&display));
    row.set_enable_expansion(true);

    let info = gtk::Image::from_icon_name("dialog-information-symbolic");
    info.set_tooltip_text(Some("Display details"));
    row.add_suffix(&info);

    let chevron = gtk::Image::from_icon_name("go-next-symbolic");
    chevron.set_tooltip_text(Some("Expand display settings"));
    row.add_suffix(&chevron);

    let resolution_row = adw::ActionRow::new();
    resolution_row.set_title("Resolution");
    let resolutions = resolution_options(display.mode_width, display.mode_height);
    let resolution_labels = resolutions
        .iter()
        .map(|(width, height)| format!("{width} x {height}"))
        .collect::<Vec<_>>();
    let resolution_label_refs = resolution_labels
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    let resolution_dropdown = dropdown_from_strings(
        &resolution_label_refs,
        resolutions
            .iter()
            .position(|resolution| *resolution == (display.mode_width, display.mode_height))
            .unwrap_or(0) as u32,
    );
    resolution_row.add_suffix(&resolution_dropdown);
    row.add_row(&resolution_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        resolution_dropdown.connect_selected_notify(move |dropdown| {
            let Some((width, height)) = resolutions.get(dropdown.selected() as usize).copied()
            else {
                return;
            };

            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.mode_width = width;
                display.mode_height = height;
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let orientation_row = adw::ActionRow::new();
    orientation_row.set_title("Orientation");
    let orientation_dropdown =
        dropdown_from_strings(ORIENTATION_OPTIONS, transform_index(&display.transform));
    orientation_row.add_suffix(&orientation_dropdown);
    row.add_row(&orientation_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        orientation_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.transform = transform_from_index(dropdown.selected()).to_string();
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let scale_row = adw::ActionRow::new();
    scale_row.set_title("Scale");
    let scale_dropdown = gtk::DropDown::from_strings(
        &SCALE_OPTIONS
            .iter()
            .map(|(label, _)| *label)
            .collect::<Vec<_>>(),
    );
    scale_dropdown.set_selected(
        SCALE_OPTIONS
            .iter()
            .position(|(_, value)| (*value - display.scale).abs() < 0.01)
            .unwrap_or(0) as u32,
    );
    scale_row.add_suffix(&scale_dropdown);
    row.add_row(&scale_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        scale_dropdown.connect_selected_notify(move |dropdown| {
            let selected = dropdown.selected() as usize;
            if let (Some(display), Some((_, scale))) = (
                displays.borrow_mut().get_mut(index),
                SCALE_OPTIONS.get(selected),
            ) {
                display.scale = *scale;
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    let primary_row = adw::ActionRow::new();
    primary_row.set_title("Primary Display");
    let primary = gtk::Switch::new();
    primary.set_active(display.primary);
    primary_row.add_suffix(&primary);
    primary_row.set_activatable_widget(Some(&primary));
    row.add_row(&primary_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        primary.connect_active_notify(move |switch| {
            let active = switch.is_active();
            let mut displays_ref = displays.borrow_mut();
            if active {
                for display in displays_ref.iter_mut() {
                    display.primary = false;
                }
            }
            if let Some(display) = displays_ref.get_mut(index) {
                display.primary = active;
            }
            drop(displays_ref);
            save_display_change(&displays, &area, &row, index);
        });
    }

    let enabled_row = adw::ActionRow::new();
    enabled_row.set_title("Enabled");
    let enabled = gtk::Switch::new();
    enabled.set_active(display.enabled);
    enabled_row.add_suffix(&enabled);
    enabled_row.set_activatable_widget(Some(&enabled));
    row.add_row(&enabled_row);

    {
        let displays = displays.clone();
        let area = area.clone();
        let row = row.clone();
        enabled.connect_active_notify(move |switch| {
            if let Some(display) = displays.borrow_mut().get_mut(index) {
                display.enabled = switch.is_active();
            }
            save_display_change(&displays, &area, &row, index);
        });
    }

    if display.hdr_supported {
        let hdr_row = adw::ActionRow::new();
        hdr_row.set_title("Enable HDR");
        hdr_row.set_subtitle("Use HDR output when this display and backend support it");
        let hdr = gtk::Switch::new();
        hdr.set_active(display.hdr_enabled);
        hdr_row.add_suffix(&hdr);
        hdr_row.set_activatable_widget(Some(&hdr));
        row.add_row(&hdr_row);

        {
            let displays = displays.clone();
            let area = area.clone();
            let row = row.clone();
            hdr.connect_active_notify(move |switch| {
                if let Some(display) = displays.borrow_mut().get_mut(index) {
                    display.hdr_enabled = display.hdr_supported && switch.is_active();
                }
                save_display_change(&displays, &area, &row, index);
            });
        }
    }

    row
}

fn persist_config(config: &FocalDeskConfig) {
    if let Err(err) = send_desktop_config(config.clone()) {
        flog_info!("settings IPC unavailable; saving config directly: {err}");
        let _ = save_config(config);
    }
}

fn persist_config_key(config: &FocalDeskConfig, key: &str, value: serde_json::Value) {
    if let Err(err) = send_desktop_set(key, value) {
        flog_info!("settings IPC set failed for {key}; saving config directly: {err}");
        let _ = save_config(config);
    }
}

fn start_config_watch(keys: &[&str]) -> mpsc::Receiver<ConfigEvent> {
    let (tx, rx) = mpsc::channel();
    let keys = keys.iter().map(|key| (*key).to_string()).collect();

    thread::spawn(move || {
        if let Err(err) = watch_desktop_keys(keys, move |response| match response {
            IpcResponse::Event { key, value } => {
                let _ = tx.send(ConfigEvent { key, value });
            }
            IpcResponse::Error { message } => {
                flog_info!("settings IPC watch error: {message}");
            }
            _ => {}
        }) {
            flog_info!("settings IPC watch unavailable: {err}");
        }
    });

    rx
}

fn set_switch_if_changed(switch: &gtk::Switch, active: bool) {
    if switch.is_active() != active {
        switch.set_active(active);
    }
}

fn set_scale_if_changed(scale: &gtk::Scale, value: f64) {
    if (scale.value() - value).abs() > f64::EPSILON {
        scale.set_value(value);
    }
}

fn main() {
    let app = adw::Application::new(Some("com.focaldesk.Settings"), Default::default());
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &adw::Application) {
    let config = Rc::new(RefCell::new(load_config()));

    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("FocalDesk Settings"));
    window.set_default_size(1000, 700);

    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar.add_top_bar(&header);

    let split = adw::NavigationSplitView::new();

    // ----- sidebar -----
    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    sidebar_box.set_margin_top(12);
    sidebar_box.set_margin_bottom(12);
    sidebar_box.set_margin_start(12);
    sidebar_box.set_margin_end(12);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);

    for name in [
        "Appearance",
        "Displays",
        "Sound",
        "Workspaces",
        "Keyboard",
        "Privacy",
        "Power",
        "Debug",
        "About",
    ] {
        let row = gtk::ListBoxRow::new();
        let label = gtk::Label::new(Some(name));
        label.set_xalign(0.0);
        label.set_margin_top(10);
        label.set_margin_bottom(10);
        label.set_margin_start(12);
        label.set_margin_end(12);

        row.set_child(Some(&label));
        list.append(&row);
    }

    sidebar_box.append(&list);

    let sidebar_page = adw::NavigationPage::new(&sidebar_box, "Settings");

    // ----- content pages -----
    let mut pages: HashMap<String, adw::NavigationPage> = HashMap::new();

    pages.insert("Appearance".to_string(), appearance_page(config.clone()));
    pages.insert("Displays".to_string(), displays_page(config.clone()));
    pages.insert("Sound".to_string(), sound_page());

    for name in [
        "Workspaces",
        "Keyboard",
        "Privacy",
        "Power",
        "Debug",
        "About",
    ] {
        let box_ = gtk::Box::new(gtk::Orientation::Vertical, 12);
        box_.set_margin_top(24);
        box_.set_margin_bottom(24);
        box_.set_margin_start(24);
        box_.set_margin_end(24);

        let title = gtk::Label::new(Some(name));
        title.add_css_class("title-1");
        title.set_xalign(0.0);

        let subtitle = gtk::Label::new(Some(&format!("{name} settings")));
        subtitle.set_xalign(0.0);

        box_.append(&title);
        box_.append(&subtitle);

        let page = adw::NavigationPage::new(&box_, name);
        pages.insert(name.to_string(), page);
    }

    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(pages.get("Appearance").unwrap()));
    split.set_content(Some(pages.get("Displays").unwrap()));

    let split_clone = split.clone();
    let pages_clone = pages.clone();

    list.connect_row_selected(move |_, row| {
        if let Some(row) = row {
            if let Some(label) = row.child().and_then(|w| w.downcast::<gtk::Label>().ok()) {
                let text = label.text().to_string();

                if let Some(page) = pages_clone.get(&text) {
                    split_clone.set_content(Some(page));
                }
            }
        }
    });

    toolbar.set_content(Some(&split));
    window.set_content(Some(&toolbar));
    window.present();
}

fn appearance_page(config: Rc<RefCell<FocalDeskConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Appearance");

    let visual_group = adw::PreferencesGroup::new();
    visual_group.set_title("Visual Style");

    // Shader chrome
    let shader_row = adw::ActionRow::new();
    shader_row.set_title("Use shader chrome");
    shader_row.set_subtitle("Enable FocalDesk beveled/glass shader styling");

    let shader_switch = gtk::Switch::new();
    shader_switch.set_active(config.borrow().appearance.shader_chrome);

    shader_row.add_suffix(&shader_switch);
    shader_row.set_activatable_widget(Some(&shader_switch));

    {
        let config = config.clone();
        shader_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().appearance.shader_chrome = active;
            persist_config_key(&config.borrow(), "appearance.shader_chrome", json!(active));
        });
    }

    visual_group.add(&shader_row);

    // Output focus glow
    let focus_row = adw::ActionRow::new();
    focus_row.set_title("Output focus glow");
    focus_row.set_subtitle("Highlight the currently focused display");

    let focus_switch = gtk::Switch::new();
    focus_switch.set_active(config.borrow().appearance.output_focus_glow);

    focus_row.add_suffix(&focus_switch);
    focus_row.set_activatable_widget(Some(&focus_switch));

    {
        let config = config.clone();
        focus_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().appearance.output_focus_glow = active;
            persist_config_key(
                &config.borrow(),
                "appearance.output_focus_glow",
                json!(active),
            );
        });
    }

    visual_group.add(&focus_row);
    page.add(&visual_group);

    let tuning_group = adw::PreferencesGroup::new();
    tuning_group.set_title("Tuning");

    // Glow strength
    let glow_row = adw::ActionRow::new();
    glow_row.set_title("Glow strength");

    let glow_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.05);
    glow_scale.set_hexpand(true);
    glow_scale.set_value(config.borrow().appearance.glow_strength);
    glow_scale.set_draw_value(true);

    let theme_row = adw::ActionRow::new();
    theme_row.set_title("Theme");

    let theme_dropdown = dropdown_from_strings(
        THEME_OPTIONS,
        THEME_OPTIONS
            .iter()
            .position(|theme| *theme == config.borrow().appearance.theme.as_str())
            .unwrap_or(0) as u32,
    );

    {
        let config = config.clone();
        theme_dropdown.connect_selected_notify(move |dropdown| {
            if let Some(theme) = THEME_OPTIONS.get(dropdown.selected() as usize) {
                let theme = (*theme).to_string();
                config.borrow_mut().appearance.theme = theme.clone();
                persist_config_key(&config.borrow(), "appearance.theme", json!(theme));
            }
        });
    }

    theme_row.add_suffix(&theme_dropdown);
    visual_group.add(&theme_row);

    {
        let config = config.clone();
        glow_scale.connect_value_changed(move |scale| {
            let value = scale.value();
            config.borrow_mut().appearance.glow_strength = value;
            persist_config_key(&config.borrow(), "appearance.glow_strength", json!(value));
        });
    }

    glow_row.add_suffix(&glow_scale);
    tuning_group.add(&glow_row);

    // Font scale
    let font_row = adw::ActionRow::new();
    font_row.set_title("Font scale");

    let font_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.75, 1.5, 0.05);
    font_scale.set_hexpand(true);
    font_scale.set_value(config.borrow().appearance.font_scale);
    font_scale.set_draw_value(true);

    {
        let config = config.clone();
        font_scale.connect_value_changed(move |scale| {
            let value = scale.value();
            config.borrow_mut().appearance.font_scale = value;
            persist_config_key(&config.borrow(), "appearance.font_scale", json!(value));
        });
    }

    font_row.add_suffix(&font_scale);
    tuning_group.add(&font_row);

    page.add(&tuning_group);

    let reset_button = gtk::Button::with_label("Reset to Defaults");
    reset_button.add_css_class("destructive-action");
    reset_button.set_halign(gtk::Align::Start);

    {
        let config = config.clone();
        reset_button.connect_clicked(move |_| {
            *config.borrow_mut() = FocalDeskConfig::default();
            persist_config(&config.borrow());
            flog_info!("Reset config");
        });
    }

    let reset_group = adw::PreferencesGroup::new();
    reset_group.add(&reset_button);
    reset_group.set_description(Some("Restore all appearance settings to their defaults"));
    page.add(&reset_group);

    {
        let rx = start_config_watch(&[
            "appearance.shader_chrome",
            "appearance.output_focus_glow",
            "appearance.theme",
            "appearance.glow_strength",
            "appearance.font_scale",
        ]);
        let config = config.clone();
        let shader_switch = shader_switch.clone();
        let focus_switch = focus_switch.clone();
        let theme_dropdown = theme_dropdown.clone();
        let glow_scale = glow_scale.clone();
        let font_scale = font_scale.clone();

        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                match event.key.as_str() {
                    "appearance.shader_chrome" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().appearance.shader_chrome = active;
                            set_switch_if_changed(&shader_switch, active);
                        }
                    }
                    "appearance.output_focus_glow" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().appearance.output_focus_glow = active;
                            set_switch_if_changed(&focus_switch, active);
                        }
                    }
                    "appearance.theme" => {
                        if let Some(theme) = event.value.as_str() {
                            config.borrow_mut().appearance.theme = theme.to_string();
                            if let Some(index) =
                                THEME_OPTIONS.iter().position(|option| *option == theme)
                            {
                                theme_dropdown.set_selected(index as u32);
                            }
                        }
                    }
                    "appearance.glow_strength" => {
                        if let Some(value) = event.value.as_f64() {
                            config.borrow_mut().appearance.glow_strength = value;
                            set_scale_if_changed(&glow_scale, value);
                        }
                    }
                    "appearance.font_scale" => {
                        if let Some(value) = event.value.as_f64() {
                            config.borrow_mut().appearance.font_scale = value;
                            set_scale_if_changed(&font_scale, value);
                        }
                    }
                    _ => {}
                }
            }

            glib::ControlFlow::Continue
        });
    }

    adw::NavigationPage::new(&page, "Appearance")
}

fn sound_page() -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Sound");

    let output_group = adw::PreferencesGroup::new();
    output_group.set_title("Output");

    let output_device_row = adw::ActionRow::new();
    output_device_row.set_title("Output Device");
    let speaker_icon = gtk::Image::from_icon_name("audio-speakers-symbolic");
    output_device_row.add_prefix(&speaker_icon);
    output_device_row.add_suffix(&dropdown_from_strings(OUTPUT_DEVICE_OPTIONS, 1));
    output_group.add(&output_device_row);

    let output_config_row = adw::ActionRow::new();
    output_config_row.set_title("Configuration");
    output_config_row.add_suffix(&dropdown_from_strings(OUTPUT_CONFIGURATION_OPTIONS, 0));
    output_group.add(&output_config_row);

    let output_volume_row = adw::ActionRow::new();
    output_volume_row.set_title("Output Volume");
    let output_volume_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    output_volume_box.set_hexpand(true);
    let output_volume_icon = gtk::Image::from_icon_name("audio-volume-high-symbolic");
    let output_volume = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 1.0);
    output_volume.set_hexpand(true);
    output_volume.set_draw_value(false);
    output_volume.set_value(75.0);
    output_volume_box.append(&output_volume_icon);
    output_volume_box.append(&output_volume);
    output_volume_row.add_suffix(&output_volume_box);
    output_group.add(&output_volume_row);

    let balance_row = adw::ActionRow::new();
    balance_row.set_title("Balance");
    let balance = gtk::Scale::with_range(gtk::Orientation::Horizontal, -1.0, 1.0, 0.05);
    balance.set_hexpand(true);
    balance.set_draw_value(false);
    balance.set_value(0.0);
    balance_row.add_suffix(&balance);
    output_group.add(&balance_row);

    let test_row = adw::ActionRow::new();
    test_row.set_title("Test Speakers");
    let test_button = gtk::Button::with_label("Test...");
    test_button.add_css_class("pill");
    test_row.add_suffix(&test_button);
    output_group.add(&test_row);

    page.add(&output_group);

    let input_group = adw::PreferencesGroup::new();
    input_group.set_title("Input");

    let input_device_row = adw::ActionRow::new();
    input_device_row.set_title("Input Device");
    input_device_row.add_suffix(&dim_label("No Input Devices"));
    input_group.add(&input_device_row);

    page.add(&input_group);

    let sounds_group = adw::PreferencesGroup::new();
    sounds_group.set_title("Sounds");

    let volume_levels_row = adw::ActionRow::new();
    volume_levels_row.set_title("Volume Levels");
    volume_levels_row.add_suffix(&suffix_chevron());
    sounds_group.add(&volume_levels_row);

    let alert_sound_row = adw::ActionRow::new();
    alert_sound_row.set_title("Alert Sound");
    alert_sound_row.add_suffix(&dropdown_from_strings(ALERT_SOUND_OPTIONS, 0));
    sounds_group.add(&alert_sound_row);

    page.add(&sounds_group);

    adw::NavigationPage::new(&page, "Sound")
}

fn displays_page(config: Rc<RefCell<FocalDeskConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Displays");

    let detected_displays = Rc::new(RefCell::new(load_displays()));
    let arrangement_group = adw::PreferencesGroup::new();
    arrangement_group.set_title("Arrangement");
    arrangement_group.set_description(Some(
        "Drag displays to arrange their logical desktop positions",
    ));

    let area = monitor_arrangement_area(detected_displays.clone());
    arrangement_group.add(&area);

    page.add(&arrangement_group);

    let outputs_group = adw::PreferencesGroup::new();
    outputs_group.set_title("Connected Displays");

    let display_count = detected_displays.borrow().len();
    if display_count == 0 {
        let row = adw::ActionRow::new();
        row.set_title("No connected displays found");
        row.set_subtitle("Display information will appear here after the compositor writes it.");
        outputs_group.add(&row);
    } else {
        for index in 0..display_count {
            let row = connected_display_row(index, detected_displays.clone(), area.clone());
            outputs_group.add(&row);
        }
    }
    page.add(&outputs_group);

    // Layout group
    let layout_group = adw::PreferencesGroup::new();
    layout_group.set_title("Layout");

    let topbar_row = adw::ActionRow::new();
    topbar_row.set_title("Show top bar on all displays");
    topbar_row.set_subtitle("When disabled, only the focused display shows the top bar");

    let topbar_switch = gtk::Switch::new();
    topbar_switch.set_active(config.borrow().displays.topbar_on_all_outputs);

    topbar_row.add_suffix(&topbar_switch);
    topbar_row.set_activatable_widget(Some(&topbar_switch));

    {
        let config = config.clone();
        topbar_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().displays.topbar_on_all_outputs = active;
            persist_config_key(
                &config.borrow(),
                "displays.topbar_on_all_outputs",
                json!(active),
            );
        });
    }

    layout_group.add(&topbar_row);

    let sidebar_row = adw::ActionRow::new();
    sidebar_row.set_title("Show sidebar on all displays");
    sidebar_row.set_subtitle("When disabled, only the focused display shows the sidebar");

    let sidebar_switch = gtk::Switch::new();
    sidebar_switch.set_active(config.borrow().displays.sidebar_on_all_outputs);

    sidebar_row.add_suffix(&sidebar_switch);
    sidebar_row.set_activatable_widget(Some(&sidebar_switch));

    {
        let config = config.clone();
        sidebar_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().displays.sidebar_on_all_outputs = active;
            persist_config_key(
                &config.borrow(),
                "displays.sidebar_on_all_outputs",
                json!(active),
            );
        });
    }

    layout_group.add(&sidebar_row);

    // Focus group
    let focus_group = adw::PreferencesGroup::new();
    focus_group.set_title("Focus");

    let remember_row = adw::ActionRow::new();
    remember_row.set_title("Remember focused display");
    remember_row.set_subtitle("Restore the last active display when FocalDesk starts");

    let remember_switch = gtk::Switch::new();
    remember_switch.set_active(config.borrow().displays.remember_focused_output);

    remember_row.add_suffix(&remember_switch);
    remember_row.set_activatable_widget(Some(&remember_switch));

    {
        let config = config.clone();
        remember_switch.connect_active_notify(move |s| {
            let active = s.is_active();
            config.borrow_mut().displays.remember_focused_output = active;
            persist_config_key(
                &config.borrow(),
                "displays.remember_focused_output",
                json!(active),
            );
        });
    }

    focus_group.add(&remember_row);

    page.add(&layout_group);
    page.add(&focus_group);

    {
        let rx = start_config_watch(&[
            "displays.topbar_on_all_outputs",
            "displays.sidebar_on_all_outputs",
            "displays.remember_focused_output",
        ]);
        let config = config.clone();
        let topbar_switch = topbar_switch.clone();
        let sidebar_switch = sidebar_switch.clone();
        let remember_switch = remember_switch.clone();

        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                match event.key.as_str() {
                    "displays.topbar_on_all_outputs" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().displays.topbar_on_all_outputs = active;
                            set_switch_if_changed(&topbar_switch, active);
                        }
                    }
                    "displays.sidebar_on_all_outputs" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().displays.sidebar_on_all_outputs = active;
                            set_switch_if_changed(&sidebar_switch, active);
                        }
                    }
                    "displays.remember_focused_output" => {
                        if let Some(active) = event.value.as_bool() {
                            config.borrow_mut().displays.remember_focused_output = active;
                            set_switch_if_changed(&remember_switch, active);
                        }
                    }
                    _ => {}
                }
            }

            glib::ControlFlow::Continue
        });
    }

    adw::NavigationPage::new(&page, "Displays")
}
