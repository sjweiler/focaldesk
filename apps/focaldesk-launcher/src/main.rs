use adw::prelude::*;
use focaldesk_logging::init_default_logging;
use focaldesk_settings_core::load_settings;
use gtk::gdk;
use gtk::glib;
use std::process::Command;
use tracing::warn;

fn sibling_binary(name: &str) -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join(name)))
        .filter(|path| path.exists())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| name.to_string())
}

fn spawn(command: &str) {
    if let Err(err) = Command::new(command).spawn() {
        warn!(command, %err, "focaldesk-launcher: failed to launch app");
    }
}

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::new(app);
    window.set_title(Some("Launcher"));
    window.set_default_size(320, 0);
    window.set_resizable(false);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(16);
    content.set_margin_bottom(16);
    content.set_margin_start(16);
    content.set_margin_end(16);

    let header = adw::HeaderBar::new();
    header.set_show_end_title_buttons(true);

    let toolbar_view = adw::ToolbarView::new();
    toolbar_view.add_top_bar(&header);
    toolbar_view.set_content(Some(&content));
    window.set_content(Some(&toolbar_view));

    let entries: [(&str, Box<dyn Fn() -> String>); 3] = [
        ("Terminal", Box::new(|| load_settings().apps.terminal)),
        ("Browser", Box::new(|| load_settings().apps.browser)),
        ("Files", Box::new(|| sibling_binary("focaldesk-files"))),
    ];

    for (label, resolve_command) in entries {
        let button = gtk::Button::with_label(label);
        button.set_height_request(40);
        let window = window.clone();
        button.connect_clicked(move |_| {
            spawn(&resolve_command());
            window.close();
        });
        content.append(&button);
    }

    let key_controller = gtk::EventControllerKey::new();
    let window_for_key = window.clone();
    key_controller.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            window_for_key.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    window.add_controller(key_controller);

    window.present();
}

fn main() {
    init_default_logging();
    let app = adw::Application::new(Some("com.focaldesk.Launcher"), Default::default());
    app.connect_activate(build_ui);
    app.run();
}
