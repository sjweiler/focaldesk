#![allow(deprecated)]

use anyhow::Result;
use focaldesk_ipc::{DialogIpcRequest, DialogIpcResponse, serve_dialog_ipc};
use focaldesk_logging::flog_info;
use gtk::prelude::*;
use std::sync::{Arc, mpsc};

fn main() -> Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();

    let app = gtk::Application::builder()
        .application_id("com.focaldesk.dialogd")
        .build();
    app.connect_activate(|app| {
        // Leaked intentionally: the guard must outlive `connect_activate`, and this
        // daemon runs until the process is killed, so there is no later point to drop it.
        Box::leak(Box::new(app.hold()));
    });

    let handler = Arc::new(move |request: DialogIpcRequest| -> DialogIpcResponse {
        let (tx, rx) = mpsc::channel();
        glib::MainContext::default().invoke(move || present_dialog(request, tx));
        rx.recv().unwrap_or_else(|_| DialogIpcResponse::Error {
            message: "dialog broker stopped".to_string(),
        })
    });

    flog_info!("FocalDesk dialog broker starting...");
    serve_dialog_ipc(handler);
    app.run();
    Ok(())
}

fn present_dialog(request: DialogIpcRequest, tx: mpsc::Sender<DialogIpcResponse>) {
    match request {
        DialogIpcRequest::AiPermissionPrompt {
            request_id,
            title,
            message,
            allow_persistent,
        } => {
            let dialog = gtk::MessageDialog::builder()
                .modal(true)
                .message_type(gtk::MessageType::Question)
                .buttons(gtk::ButtonsType::None)
                .text(title)
                .secondary_text(message)
                .build();
            dialog.add_button("Deny", gtk::ResponseType::Cancel);
            dialog.add_button("Allow", gtk::ResponseType::Accept);
            if allow_persistent {
                dialog.add_button("Remember", gtk::ResponseType::Other(1));
            }

            dialog.connect_response(move |dialog, response| {
                let (allow, persistent) = match response {
                    gtk::ResponseType::Accept => (true, false),
                    gtk::ResponseType::Other(1) => (true, true),
                    _ => (false, false),
                };
                let _ = tx.send(DialogIpcResponse::AiPermissionDecision {
                    request_id,
                    allow,
                    persistent,
                });
                dialog.close();
            });

            dialog.present();
        }
        DialogIpcRequest::PortalChooserPrompt {
            request_id,
            title,
            message,
            choices,
        } => {
            let dialog = gtk::MessageDialog::builder()
                .modal(true)
                .message_type(gtk::MessageType::Question)
                .buttons(gtk::ButtonsType::None)
                .text(title)
                .secondary_text(message)
                .build();
            dialog.add_button("Cancel", gtk::ResponseType::Cancel);

            for (idx, choice) in choices.iter().enumerate() {
                dialog.add_button(choice, gtk::ResponseType::Other((idx + 1) as u16));
            }

            dialog.connect_response(move |dialog, response| {
                let selected = match response {
                    gtk::ResponseType::Other(choice_idx) if choice_idx > 0 => Some(
                        choices
                            .get((choice_idx - 1) as usize)
                            .cloned()
                            .unwrap_or_default(),
                    ),
                    _ => None,
                };

                let _ = tx.send(DialogIpcResponse::PortalChooserDecision {
                    request_id,
                    selected,
                });
                dialog.close();
            });

            dialog.present();
        }
        DialogIpcRequest::PolkitAuthPrompt {
            request_id,
            message,
            icon_name,
            prompt,
            echo_on,
        } => {
            let dialog = gtk::Dialog::builder()
                .modal(true)
                .title(&message)
                .build();
            if !icon_name.is_empty() {
                dialog.set_icon_name(Some(&icon_name));
            }

            let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
            content.set_margin_top(16);
            content.set_margin_bottom(16);
            content.set_margin_start(16);
            content.set_margin_end(16);

            let label = gtk::Label::new(Some(&prompt));
            label.set_wrap(true);
            label.set_xalign(0.0);
            content.append(&label);

            if echo_on {
                let entry = gtk::Entry::builder()
                    .placeholder_text("Response")
                    .activates_default(true)
                    .build();
                content.append(&entry);

                let cancel_button = dialog.add_button("Cancel", gtk::ResponseType::Cancel);
                let ok_button = dialog.add_button("OK", gtk::ResponseType::Ok);
                dialog.set_default_widget(Some(&ok_button));
                let _ = cancel_button;

                dialog.connect_response(move |dialog, response| {
                    let answer = match response {
                        gtk::ResponseType::Ok => Some(entry.text().to_string()),
                        _ => None,
                    };
                    let _ = tx.send(DialogIpcResponse::PolkitAuthAnswer { request_id, answer });
                    dialog.close();
                });
            } else {
                let entry = gtk::PasswordEntry::builder()
                    .placeholder_text("Password")
                    .activates_default(true)
                    .show_peek_icon(false)
                    .build();
                content.append(&entry);

                let cancel_button = dialog.add_button("Cancel", gtk::ResponseType::Cancel);
                let ok_button = dialog.add_button("OK", gtk::ResponseType::Ok);
                dialog.set_default_widget(Some(&ok_button));
                let _ = cancel_button;

                dialog.connect_response(move |dialog, response| {
                    let answer = match response {
                        gtk::ResponseType::Ok => Some(entry.text().to_string()),
                        _ => None,
                    };
                    let _ = tx.send(DialogIpcResponse::PolkitAuthAnswer { request_id, answer });
                    dialog.close();
                });
            }

            dialog.content_area().append(&content);
            dialog.present();
        }
    }
}
