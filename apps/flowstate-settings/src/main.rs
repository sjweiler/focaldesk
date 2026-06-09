mod config;

use adw::prelude::*;
use flowstate_logging::flog_info;
use gtk::prelude::*;

use config::{load_config, save_config, FlowStateConfig};
use gtk::cairo;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

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
}

fn save_displays(displays: &[DisplayConfig]) {
    let path = displays_path();

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(text) = serde_json::to_string_pretty(displays) {
        let _ = std::fs::write(path, text);
    }
}

fn display_preview_rect(
    d: &DisplayConfig,
    zoom: f64,
    offset_x: f64,
    offset_y: f64,
) -> (f64, f64, f64, f64) {
    let logical_w = d.mode_width as f64 / d.scale.max(1.0);
    let logical_h = d.mode_height as f64 / d.scale.max(1.0);

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
        .join("flowstate")
        .join("displays.json")
}

fn load_displays() -> Vec<DisplayConfig> {
    let path = displays_path();

    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => vec![],
    }
}

fn main() {
    let app = adw::Application::new(Some("com.flowstate.Settings"), Default::default());
    app.connect_activate(build_ui);
    app.run();
}

fn build_ui(app: &adw::Application) {
    let config = Rc::new(RefCell::new(load_config()));

    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("FlowState Settings"));
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

fn appearance_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Appearance");

    let visual_group = adw::PreferencesGroup::new();
    visual_group.set_title("Visual Style");

    // Shader chrome
    let shader_row = adw::ActionRow::new();
    shader_row.set_title("Use shader chrome");
    shader_row.set_subtitle("Enable FlowState beveled/glass shader styling");

    let shader_switch = gtk::Switch::new();
    shader_switch.set_active(config.borrow().appearance.shader_chrome);

    shader_row.add_suffix(&shader_switch);
    shader_row.set_activatable_widget(Some(&shader_switch));

    {
        let config = config.clone();
        shader_switch.connect_active_notify(move |s| {
            config.borrow_mut().appearance.shader_chrome = s.is_active();
            let _ = save_config(&config.borrow());
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
            config.borrow_mut().appearance.output_focus_glow = s.is_active();
            let _ = save_config(&config.borrow());
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

    let combo = gtk::ComboBoxText::new();
    combo.append_text("Eagle");
    combo.append_text("Moonbase");
    combo.append_text("Classic");

    combo.set_active(Some(match config.borrow().appearance.theme.as_str() {
        "Moonbase" => 1,
        "Classic" => 2,
        _ => 0,
    }));

    {
        let config = config.clone();
        combo.connect_changed(move |c| {
            if let Some(text) = c.active_text() {
                config.borrow_mut().appearance.theme = text.to_string();
                let _ = save_config(&config.borrow());
            }
        });
    }

    theme_row.add_suffix(&combo);
    visual_group.add(&theme_row);

    {
        let config = config.clone();
        glow_scale.connect_value_changed(move |scale| {
            config.borrow_mut().appearance.glow_strength = scale.value();
            let _ = save_config(&config.borrow());
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
            config.borrow_mut().appearance.font_scale = scale.value();
            let _ = save_config(&config.borrow());
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
            *config.borrow_mut() = FlowStateConfig::default();
            let _ = save_config(&config.borrow());
            flog_info!("Reset config");
        });
    }

    let reset_group = adw::PreferencesGroup::new();
    reset_group.add(&reset_button);
    reset_group.set_description(Some("Restore all appearance settings to their defaults"));
    page.add(&reset_group);

    adw::NavigationPage::new(&page, "Appearance")
}

fn displays_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
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

    let displays = load_displays();
    let outputs_group = adw::PreferencesGroup::new();
    outputs_group.set_title("Connected Displays");

    for d in displays {
        let row = adw::ActionRow::new();

        row.set_title(&d.name);

        row.set_subtitle(&format!(
            "{}×{} @ {} Hz  •  Scale {:.2}  •  Pos {}, {}{}",
            d.mode_width,
            d.mode_height,
            d.refresh_mhz,
            d.scale,
            d.logical_x,
            d.logical_y,
            if d.primary { "  •  Primary" } else { "" }
        ));

        outputs_group.add(&row);
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
            config.borrow_mut().displays.topbar_on_all_outputs = s.is_active();
            let _ = save_config(&config.borrow());
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
            config.borrow_mut().displays.sidebar_on_all_outputs = s.is_active();
            let _ = save_config(&config.borrow());
        });
    }

    layout_group.add(&sidebar_row);

    // Focus group
    let focus_group = adw::PreferencesGroup::new();
    focus_group.set_title("Focus");

    let remember_row = adw::ActionRow::new();
    remember_row.set_title("Remember focused display");
    remember_row.set_subtitle("Restore the last active display when FlowState starts");

    let remember_switch = gtk::Switch::new();
    remember_switch.set_active(config.borrow().displays.remember_focused_output);

    remember_row.add_suffix(&remember_switch);
    remember_row.set_activatable_widget(Some(&remember_switch));

    {
        let config = config.clone();
        remember_switch.connect_active_notify(move |s| {
            config.borrow_mut().displays.remember_focused_output = s.is_active();
            let _ = save_config(&config.borrow());
        });
    }

    focus_group.add(&remember_row);

    page.add(&layout_group);
    page.add(&focus_group);

    adw::NavigationPage::new(&page, "Displays")
}

fn workspaces_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Workspaces");

    adw::NavigationPage::new(&page, "Workspaces")
}

fn keyboard_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Keyboard");

    adw::NavigationPage::new(&page, "Keyboard")
}

fn privacy_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Privacy");

    adw::NavigationPage::new(&page, "Privacy")
}

fn power_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Power");

    adw::NavigationPage::new(&page, "Power")
}

fn debug_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("Debug");

    adw::NavigationPage::new(&page, "Debug")
}

fn about_page(config: Rc<RefCell<FlowStateConfig>>) -> adw::NavigationPage {
    let page = adw::PreferencesPage::new();
    page.set_title("About");

    adw::NavigationPage::new(&page, "About")
}
