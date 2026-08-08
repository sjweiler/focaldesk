use anyhow::Context;
use focaldesk_ai::{
    AiDaemonStatus, AiIpcRequest, AiIpcResponse, ChatMessage, ChatRequest, MemoryId, ProviderInfo,
    ProviderModelInfo, SearchHit, send_ai_request,
};
use focaldesk_config::load_config;
use focaldesk_gtk::{StateKind, StateView, StatusBanner};
use focaldesk_ipc::{
    IpcRequest, IpcResponse, NotificationIpcRequest, NotificationIpcResponse, send_desktop_request,
    send_notification_request,
};
use focaldesk_settings_core::load_settings;
use focaldesk_themes::{GtkAppThemeOptions, gtk_app_css, gtk_app_prefers_dark, theme_by_name};
use focaldesk_voice::{VoiceEvent, VoiceSession};
use glib::ControlFlow;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box, Button, ComboBoxText, Entry, Label, Orientation, Paned,
    Revealer, ScrolledWindow, Switch, TextBuffer, TextView,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
struct Conversation {
    title: String,
    summary: String,
    messages: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct AppState {
    active_conversation: usize,
    active_provider: String,
    active_model: String,
    memory_notes: Vec<String>,
    compact_sidebar: bool,
    show_timestamps: bool,
    auto_scroll: bool,
    verbose_output: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct PersistedState {
    conversations: Vec<Conversation>,
    app_state: AppState,
}

#[derive(Clone, Default)]
struct AiConsoleRuntime {
    providers: Vec<ProviderInfo>,
    provider_models: BTreeMap<String, Vec<ProviderModelInfo>>,
    provider_model_errors: BTreeMap<String, String>,
    default_provider: Option<String>,
    status: Option<AiDaemonStatus>,
    load_error: Option<String>,
}

struct ProvidersPage {
    page: Box,
    summary_box: Box,
    list_box: Box,
    store: Rc<RefCell<PersistedState>>,
    runtime: Rc<RefCell<AiConsoleRuntime>>,
    quick_prompts_page: Rc<QuickPromptsPage>,
    composer_status_label: Label,
    log_buffer: TextBuffer,
}

#[derive(Default)]
struct PromptActivity {
    last_label: Option<String>,
    last_request: Option<String>,
    last_response: Option<String>,
    last_error: Option<String>,
    active_provider: Option<String>,
    active_model: Option<String>,
    in_flight: bool,
}

struct QuickPromptsPage {
    page: Box,
    activity_box: Box,
    detail_box: Box,
    state: Rc<RefCell<PromptActivity>>,
}

#[derive(Clone)]
struct BackendBannerHandles {
    title_label: Label,
    subtitle_label: Label,
    backend_combo: ComboBoxText,
    model_combo: ComboBoxText,
    provider_combo_syncing: Rc<RefCell<bool>>,
    model_combo_syncing: Rc<RefCell<bool>>,
}

impl BackendBannerHandles {
    fn refresh(&self, state: &PersistedState, runtime: &AiConsoleRuntime) {
        // Repopulating the combos below fires GTK's "changed" signal
        // synchronously. Without these guards that reenters the
        // connect_changed handlers while callers of refresh() (e.g. the
        // async runtime refresh) are still holding a borrow on `state`,
        // which panics with "RefCell already borrowed" and aborts the
        // process since the panic crosses a GTK callback boundary.
        *self.provider_combo_syncing.borrow_mut() = true;
        *self.model_combo_syncing.borrow_mut() = true;
        refresh_backend_banner(
            &self.title_label,
            &self.subtitle_label,
            &self.backend_combo,
            &self.model_combo,
            state,
            runtime,
        );
        *self.provider_combo_syncing.borrow_mut() = false;
        *self.model_combo_syncing.borrow_mut() = false;
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            active_conversation: 0,
            active_provider: String::new(),
            active_model: String::new(),
            memory_notes: Vec::new(),
            compact_sidebar: true,
            show_timestamps: true,
            auto_scroll: true,
            verbose_output: false,
        }
    }
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            conversations: vec![Conversation {
                title: "New Chat 1".to_string(),
                summary: "Empty thread".to_string(),
                messages: Vec::new(),
            }],
            app_state: AppState::default(),
        }
    }
}

fn load_ai_runtime() -> AiConsoleRuntime {
    let mut runtime = AiConsoleRuntime::default();

    match send_ai_request(&AiIpcRequest::ListProviders) {
        Ok(AiIpcResponse::Providers {
            default_provider,
            providers,
        }) => {
            runtime.default_provider = Some(default_provider);
            runtime.providers = providers;
            for provider in &runtime.providers {
                match send_ai_request(&AiIpcRequest::ListModels {
                    provider: provider.id.clone(),
                }) {
                    Ok(AiIpcResponse::Models { provider, models }) => {
                        runtime.provider_models.insert(provider, models);
                    }
                    Ok(AiIpcResponse::Error { message }) => {
                        runtime
                            .provider_model_errors
                            .insert(provider.id.clone(), message);
                    }
                    Ok(other) => {
                        runtime.provider_model_errors.insert(
                            provider.id.clone(),
                            format!("unexpected AI response: {other:?}"),
                        );
                    }
                    Err(err) => {
                        runtime
                            .provider_model_errors
                            .insert(provider.id.clone(), err.to_string());
                    }
                }
            }
        }
        Ok(AiIpcResponse::Error { message }) => {
            runtime.load_error = Some(message);
        }
        Ok(other) => {
            runtime.load_error = Some(format!("unexpected AI response: {other:?}"));
        }
        Err(err) => {
            runtime.load_error = Some(err.to_string());
        }
    }

    match send_ai_request(&AiIpcRequest::Status) {
        Ok(AiIpcResponse::Status { status }) => runtime.status = Some(status),
        Ok(AiIpcResponse::Error { message }) => runtime.load_error = Some(message),
        Ok(other) => {
            runtime.load_error = Some(format!("unexpected AI response: {other:?}"));
        }
        Err(err) => {
            runtime.load_error = Some(err.to_string());
        }
    }

    runtime
}

fn normalize_state_with_runtime(state: &mut PersistedState, runtime: &AiConsoleRuntime) {
    if state.app_state.active_provider.is_empty() {
        if let Some(default_provider) = runtime.default_provider.as_ref() {
            state.app_state.active_provider = default_provider.clone();
        }
    }

    if !runtime.providers.is_empty()
        && !runtime
            .providers
            .iter()
            .any(|provider| provider.id == state.app_state.active_provider)
    {
        if let Some(default_provider) = runtime.default_provider.as_ref() {
            if runtime
                .providers
                .iter()
                .any(|provider| provider.id == *default_provider)
            {
                state.app_state.active_provider = default_provider.clone();
            } else if let Some(first) = runtime.providers.first() {
                state.app_state.active_provider = first.id.clone();
            }
        } else if let Some(first) = runtime.providers.first() {
            state.app_state.active_provider = first.id.clone();
        }
    }

    if is_placeholder_model_name(&state.app_state.active_model) {
        state.app_state.active_model.clear();
    }

    sync_active_model_with_provider(state, runtime);
}

fn is_placeholder_conversation(conversation: &Conversation) -> bool {
    conversation.title.starts_with("New Chat")
        && conversation.summary == "Empty thread"
        && conversation.messages.is_empty()
}

fn is_placeholder_model_name(model: &str) -> bool {
    matches!(
        model.trim().to_ascii_lowercase().as_str(),
        "" | "default" | "default model" | "unset" | "unknown"
    )
}

fn effective_model_label(state: &PersistedState, runtime: &AiConsoleRuntime) -> String {
    effective_runtime_model(state, runtime).unwrap_or_else(|| "unset".to_string())
}

fn effective_runtime_model(state: &PersistedState, runtime: &AiConsoleRuntime) -> Option<String> {
    if !is_placeholder_model_name(&state.app_state.active_model) {
        return Some(state.app_state.active_model.clone());
    }

    selected_model_for_provider(runtime, &state.app_state.active_provider)
}

fn effective_request_model(state: &PersistedState) -> Option<String> {
    if is_placeholder_model_name(&state.app_state.active_model) {
        None
    } else {
        Some(state.app_state.active_model.clone())
    }
}

fn sync_active_model_with_provider(state: &mut PersistedState, runtime: &AiConsoleRuntime) {
    let provider_id = state.app_state.active_provider.clone();
    let installed_models = runtime
        .provider_models
        .get(&provider_id)
        .cloned()
        .unwrap_or_default();

    if installed_models.is_empty() {
        state.app_state.active_model = runtime
            .providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .and_then(|provider| provider.default_model.clone())
            .unwrap_or_default();
        if state.app_state.active_model.is_empty() {
            state.app_state.active_model.clear();
        }
        return;
    }

    if !state.app_state.active_model.is_empty()
        && installed_models
            .iter()
            .any(|model| model.id == state.app_state.active_model)
    {
        return;
    }

    if let Some(default_model) = runtime
        .providers
        .iter()
        .find(|provider| provider.id == provider_id)
        .and_then(|provider| provider.default_model.clone())
        .filter(|default_model| {
            installed_models
                .iter()
                .any(|model| model.id == *default_model)
        })
    {
        state.app_state.active_model = default_model;
        return;
    }

    if let Some(first_model) = installed_models.first() {
        state.app_state.active_model = first_model.id.clone();
    }
}

fn selected_model_for_provider(runtime: &AiConsoleRuntime, provider_id: &str) -> Option<String> {
    runtime.provider_models.get(provider_id).and_then(|models| {
        models.first().map(|model| model.id.clone()).or_else(|| {
            runtime
                .providers
                .iter()
                .find(|provider| provider.id == provider_id)
                .and_then(|provider| provider.default_model.clone())
        })
    })
}

fn provider_models_for(runtime: &AiConsoleRuntime, provider_id: &str) -> Vec<ProviderModelInfo> {
    runtime
        .provider_models
        .get(provider_id)
        .cloned()
        .unwrap_or_default()
}

fn model_label(model: &ProviderModelInfo) -> String {
    model.id.clone()
}

fn main() {
    let app = Application::builder()
        .application_id("dev.focaldesk.AiConsole")
        .build();

    let main_window = Rc::new(RefCell::new(None));
    let main_window_for_activate = main_window.clone();
    app.connect_activate(move |app| build_ui(app, main_window_for_activate.clone()));
    app.run();
}

fn build_ui(app: &Application, main_window: Rc<RefCell<Option<ApplicationWindow>>>) {
    if let Some(window) = main_window.borrow().as_ref() {
        window.present();
        return;
    }

    load_css();

    let state = Rc::new(RefCell::new(load_state()));
    let runtime = Rc::new(RefCell::new(AiConsoleRuntime::default()));
    let log_buffer = TextBuffer::new(None);
    let quick_prompts_page = build_quick_prompts_page();
    sync_quick_prompts_backend(&quick_prompts_page, &state.borrow(), &runtime.borrow());

    let window = ApplicationWindow::builder()
        .application(app)
        .title("FocalDesk AI Console")
        .default_width(950)
        .default_height(650)
        .build();
    window.add_css_class("focaldesk-app");
    *main_window.borrow_mut() = Some(window.clone());
    {
        let main_window = main_window.clone();
        window.connect_close_request(move |_| {
            main_window.borrow_mut().take();
            glib::Propagation::Proceed
        });
    }

    let root = Box::new(Orientation::Horizontal, 12);
    root.add_css_class("ai-root");

    let sidebar = Box::new(Orientation::Vertical, 8);
    sidebar.add_css_class("ai-sidebar");
    sidebar.set_width_request(210);

    let nav_items = [
        "New Chat",
        "Conversations",
        "Providers",
        "Quick Prompts",
        "Memory",
        "Settings",
        "Log/Debug",
    ];
    let active_nav = Rc::new(std::cell::RefCell::new(String::from("New Chat")));
    let nav_buttons = Rc::new(std::cell::RefCell::new(Vec::<Button>::new()));

    for item in nav_items {
        let btn = Button::with_label(item);
        btn.add_css_class("sidebar-button");
        if item == "New Chat" {
            btn.add_css_class("sidebar-button-active");
        }
        nav_buttons.borrow_mut().push(btn.clone());
        sidebar.append(&btn);
    }

    let main = Box::new(Orientation::Vertical, 10);
    main.add_css_class("ai-main");
    main.set_vexpand(true);

    let composer_status_label = Label::new(None);
    composer_status_label.set_xalign(0.0);
    composer_status_label.add_css_class("mode-banner-body");
    composer_status_label.add_css_class("composer-status");

    let composer = Box::new(Orientation::Horizontal, 8);
    composer.add_css_class("composer");
    composer.set_hexpand(true);
    composer.set_vexpand(false);

    let entry = Entry::builder()
        .placeholder_text("type message here...")
        .hexpand(true)
        .build();

    let send = Button::with_label("Send");
    let voice_button = Button::with_label("Voice");

    let stack = gtk4::Stack::new();
    stack.set_hexpand(true);
    stack.set_vexpand(true);
    let stack_scroll = ScrolledWindow::builder()
        .child(&stack)
        .hexpand(true)
        .vexpand(true)
        .build();
    stack_scroll.add_css_class("pane-scroll");

    let chat_view = Box::new(Orientation::Vertical, 10);
    chat_view.add_css_class("chat-list");
    let conversation_detail = Box::new(Orientation::Vertical, 6);
    conversation_detail.add_css_class("item-card");
    conversation_detail.add_css_class("conversation-detail");
    conversation_detail.add_css_class("detail-pane");
    conversation_detail.set_vexpand(true);
    conversation_detail.set_width_request(360);

    {
        let snapshot = state.borrow();
        load_active_conversation(&chat_view, &snapshot.conversations, &snapshot.app_state);
        load_log_buffer(&log_buffer, &snapshot);
        if let Some(conversation) = snapshot
            .conversations
            .get(snapshot.app_state.active_conversation)
            .or_else(|| snapshot.conversations.first())
        {
            render_conversation_panel(&conversation_detail, conversation, "Active conversation");
        }
    }

    let chat_scroll = ScrolledWindow::builder()
        .child(&chat_view)
        .vexpand(true)
        .hexpand(true)
        .build();
    chat_scroll.add_css_class("transcript-pane");

    let transcript_column = Box::new(Orientation::Vertical, 8);
    transcript_column.set_hexpand(true);
    transcript_column.set_vexpand(true);
    let transcript_header = Box::new(Orientation::Horizontal, 8);
    let transcript_label = Label::new(Some("Transcript"));
    transcript_label.set_xalign(0.0);
    transcript_label.add_css_class("pane-heading");
    transcript_label.set_hexpand(true);
    let new_chat_button = Button::with_label("New conversation");
    new_chat_button.add_css_class("sidebar-button");
    let detail_toggle = Button::with_label("Show details");
    detail_toggle.add_css_class("sidebar-button");
    transcript_header.append(&transcript_label);
    transcript_header.append(&new_chat_button);
    transcript_header.append(&detail_toggle);
    transcript_column.append(&transcript_header);
    transcript_column.append(&chat_scroll);

    let detail_scroll = ScrolledWindow::builder()
        .child(&conversation_detail)
        .vexpand(true)
        .hexpand(false)
        .build();
    detail_scroll.add_css_class("pane-scroll");

    let detail_column = Box::new(Orientation::Vertical, 8);
    detail_column.set_vexpand(true);
    detail_column.set_width_request(320);
    let detail_label = Label::new(Some("Active thread"));
    detail_label.set_xalign(0.0);
    detail_label.add_css_class("pane-heading");
    detail_column.append(&detail_label);
    detail_column.append(&detail_scroll);

    let detail_revealer = Revealer::new();
    detail_revealer.set_child(Some(&detail_column));
    detail_revealer.set_reveal_child(false);

    {
        let detail_revealer = detail_revealer.clone();
        let detail_toggle_state = detail_toggle.clone();
        detail_toggle.clone().connect_clicked(move |_| {
            let reveal = !detail_revealer.reveals_child();
            detail_revealer.set_reveal_child(reveal);
            detail_toggle_state.set_label(if reveal {
                "Hide details"
            } else {
                "Show details"
            });
        });
    }

    {
        let state = state.clone();
        let chat_view = chat_view.clone();
        let conversation_detail = conversation_detail.clone();
        let log_buffer = log_buffer.clone();
        new_chat_button.connect_clicked(move |_| {
            create_new_conversation(&state, &chat_view, &conversation_detail, &log_buffer);
        });
    }

    let new_chat_page = Paned::new(Orientation::Horizontal);
    new_chat_page.add_css_class("split-pane");
    new_chat_page.set_start_child(Some(&transcript_column));
    new_chat_page.set_end_child(Some(&detail_revealer));
    new_chat_page.set_position(980);
    new_chat_page.set_wide_handle(true);
    new_chat_page.set_vexpand(true);

    let new_chat_workspace = Box::new(Orientation::Vertical, 10);
    new_chat_workspace.set_hexpand(true);
    new_chat_workspace.set_vexpand(true);
    new_chat_workspace.append(&new_chat_page);

    refresh_composer_status_label(&composer_status_label, &state.borrow(), &runtime.borrow());

    let providers_page = build_providers_page(
        state.clone(),
        runtime.clone(),
        quick_prompts_page.clone(),
        composer_status_label.clone(),
        log_buffer.clone(),
    );
    let (mode_banner, banner_handles) = build_backend_banner(
        state.clone(),
        runtime.clone(),
        log_buffer.clone(),
        stack.clone(),
        providers_page.clone(),
        quick_prompts_page.clone(),
        composer_status_label.clone(),
    );

    main.append(&mode_banner);
    stack.add_titled(
        &conversations_page(
            state.clone(),
            chat_view.clone(),
            conversation_detail.clone(),
            stack.clone(),
            composer.clone(),
            active_nav.clone(),
            nav_buttons.clone(),
            log_buffer.clone(),
        ),
        Some("conversations"),
        "Conversations",
    );
    stack.add_titled(&providers_page.page, Some("providers"), "Providers");
    stack.add_titled(
        &tools_page(
            chat_view.clone(),
            conversation_detail.clone(),
            stack.clone(),
            composer.clone(),
            entry.clone(),
            send.clone(),
            active_nav.clone(),
            nav_buttons.clone(),
            state.clone(),
            quick_prompts_page.clone(),
            log_buffer.clone(),
        ),
        Some("prompts"),
        "Quick Prompts",
    );
    stack.add_titled(
        &memory_page(state.clone(), log_buffer.clone()),
        Some("memory"),
        "Memory",
    );
    stack.add_titled(
        &settings_page(state.clone(), log_buffer.clone()),
        Some("settings"),
        "Settings",
    );
    stack.add_titled(&debug_page(log_buffer.clone()), Some("debug"), "Log/Debug");

    stack.set_visible_child_name("new-chat");

    let entry_clone = entry.clone();
    let state_clone = state.clone();
    let chat_view_clone = chat_view.clone();
    let conversation_detail_clone = conversation_detail.clone();
    let log_buffer_clone = log_buffer.clone();
    let send_button = send.clone();
    send.connect_clicked(move |_| {
        let text = entry_clone.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        dispatch_chat_request_async(
            state_clone.clone(),
            chat_view_clone.clone(),
            conversation_detail_clone.clone(),
            entry_clone.clone(),
            send_button.clone(),
            log_buffer_clone.clone(),
            text,
            "manual chat",
            None,
        );
    });

    {
        let state = state.clone();
        let chat_view = chat_view.clone();
        let conversation_detail = conversation_detail.clone();
        let entry = entry.clone();
        let send_button = send.clone();
        let log_buffer = log_buffer.clone();
        entry.connect_activate(move |entry| {
            let text = entry.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            dispatch_chat_request_async(
                state.clone(),
                chat_view.clone(),
                conversation_detail.clone(),
                entry.clone(),
                send_button.clone(),
                log_buffer.clone(),
                text,
                "manual chat",
                None,
            );
        });
    }

    {
        let current_session: Rc<RefCell<Option<VoiceSession>>> = Rc::new(RefCell::new(None));
        let entry = entry.clone();
        let log_buffer = log_buffer.clone();
        voice_button.connect_clicked(move |button| {
            if let Some(session) = current_session.borrow().as_ref() {
                session.stop();
                button.set_label("Voice");
                // current_session is cleared by the polling loop once it observes the
                // channel disconnect, so the trailing final phrase still gets applied.
                return;
            }

            let model_dir = match focaldesk_voice::find_model_dir() {
                Some(dir) => dir,
                None => {
                    append_log(
                        &log_buffer,
                        &format!("[voice] {}", focaldesk_voice::install_instructions()),
                    );
                    return;
                }
            };

            let (tx, rx) = mpsc::channel::<VoiceEvent>();
            let session = match VoiceSession::start(model_dir, tx) {
                Ok(session) => session,
                Err(err) => {
                    append_log(&log_buffer, &format!("[voice] failed to start: {err}"));
                    return;
                }
            };
            *current_session.borrow_mut() = Some(session);
            button.set_label("Stop");

            let base_text = entry.text().to_string();
            let mut base_text = base_text.trim_end().to_string();
            if !base_text.is_empty() {
                base_text.push(' ');
            }
            let mut accumulated = String::new();

            let entry_for_poll = entry.clone();
            let log_buffer_for_poll = log_buffer.clone();
            let button_for_poll = button.clone();
            let current_session_for_poll = current_session.clone();
            glib::timeout_add_local(Duration::from_millis(80), move || {
                loop {
                    match rx.try_recv() {
                        Ok(VoiceEvent::Ready) => {
                            append_log(&log_buffer_for_poll, "[voice] microphone ready");
                        }
                        Ok(VoiceEvent::Partial(partial)) => {
                            entry_for_poll.set_text(&format!("{base_text}{accumulated}{partial}"));
                            entry_for_poll.set_position(-1);
                        }
                        Ok(VoiceEvent::Final(text)) => {
                            if !text.is_empty() {
                                accumulated.push_str(&text);
                                accumulated.push(' ');
                            }
                            entry_for_poll.set_text(&format!("{base_text}{accumulated}"));
                            entry_for_poll.set_position(-1);
                        }
                        Ok(VoiceEvent::Error(err)) => {
                            append_log(&log_buffer_for_poll, &format!("[voice] {err}"));
                            button_for_poll.set_label("Voice");
                            *current_session_for_poll.borrow_mut() = None;
                            return ControlFlow::Break;
                        }
                        Err(mpsc::TryRecvError::Empty) => return ControlFlow::Continue,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            *current_session_for_poll.borrow_mut() = None;
                            return ControlFlow::Break;
                        }
                    }
                }
            });
        });
    }

    composer.append(&composer_status_label);
    composer.append(&entry);
    composer.append(&voice_button);
    composer.append(&send);

    stack.add_titled(&new_chat_workspace, Some("new-chat"), "New Chat");

    main.append(&stack_scroll);
    main.append(&composer);

    let stack_clone = stack.clone();
    let active_nav_clone = active_nav.clone();
    let nav_buttons_clone = nav_buttons.clone();
    for button in nav_buttons.borrow().iter() {
        let label = button.label().unwrap_or_default();
        let stack = stack_clone.clone();
        let active_nav = active_nav_clone.clone();
        let nav_buttons = nav_buttons_clone.clone();
        let state = state.clone();
        let chat_view = chat_view.clone();

        button.connect_clicked(move |_| {
            set_active_nav(&label, &active_nav, &nav_buttons);
            let page_name = match label.as_str() {
                "New Chat" => "new-chat",
                "Conversations" => "conversations",
                "Providers" => "providers",
                "Quick Prompts" => "prompts",
                "Memory" => "memory",
                "Settings" => "settings",
                "Log/Debug" => "debug",
                _ => "new-chat",
            };

            if label.as_str() == "New Chat" {
                // Navigation should not manufacture a conversation.
                render_active_conversation(&chat_view, &state.borrow());
            }

            stack.set_visible_child_name(page_name);
        });
    }

    root.append(&sidebar);
    root.append(&main);

    window.set_child(Some(&root));
    window.present();

    append_log(
        &log_buffer,
        "[startup] window shown; scheduling async AI runtime refresh",
    );

    refresh_ai_runtime_async(
        runtime.clone(),
        state.clone(),
        banner_handles.clone(),
        providers_page.clone(),
        quick_prompts_page.clone(),
        composer_status_label.clone(),
        log_buffer.clone(),
        stack.clone(),
        false,
        "startup",
    );
}

fn add_message(parent: &Box, text: &str, class_name: &str) {
    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("chat-card");
    label.add_css_class(class_name);
    parent.append(&label);
}

fn load_conversation(chat_view: &Box, conversation: &Conversation) {
    render_conversation_panel(chat_view, conversation, "Conversation");
}

fn render_conversation_panel(parent: &Box, conversation: &Conversation, heading: &str) {
    clear_box(parent);
    add_message(
        parent,
        &format!("{heading}: {}", conversation.title),
        "action-card",
    );
    add_message(
        parent,
        &format!("Summary: {}", conversation.summary),
        "item-card",
    );
    if conversation.messages.is_empty() {
        add_message(parent, "No messages yet.", "item-card");
    }
    for message in &conversation.messages {
        if message.starts_with("User:") {
            add_message(parent, &message, "user-card");
        } else {
            add_message(parent, &message, "ai-card");
        }
    }
}

fn load_log_buffer(buffer: &TextBuffer, store: &PersistedState) {
    let mut text = String::from(
        "[info] ai-console booted\n[info] sidebar nav ready\n[info] runtime refresh queued asynchronously\n",
    );
    text.push_str(&format!(
        "[debug] provider={}\n[debug] model={}\n[debug] active_conversation={}\n[debug] conversation_count={}\n[debug] auto_scroll={} verbose_output={}\n",
        store.app_state.active_provider,
        store.app_state.active_model,
        store.app_state.active_conversation,
        store.conversations.len(),
        store.app_state.auto_scroll,
        store.app_state.verbose_output,
    ));
    buffer.set_text(&text);
}

fn build_backend_banner(
    state: Rc<RefCell<PersistedState>>,
    runtime: Rc<RefCell<AiConsoleRuntime>>,
    log_buffer: TextBuffer,
    stack: gtk4::Stack,
    providers_page: Rc<ProvidersPage>,
    quick_prompts_page: Rc<QuickPromptsPage>,
    composer_status_label: Label,
) -> (Box, Rc<BackendBannerHandles>) {
    let banner = Box::new(Orientation::Vertical, 8);
    banner.add_css_class("mode-banner");

    let title_label = Label::new(Some("AI Console"));
    title_label.set_xalign(0.0);
    title_label.add_css_class("mode-banner-title");

    let subtitle_label = Label::new(None);
    subtitle_label.set_xalign(0.0);
    subtitle_label.add_css_class("mode-banner-body");

    let controls = Box::new(Orientation::Horizontal, 10);

    let backend_label = Label::new(Some("Provider"));
    backend_label.set_xalign(0.0);
    backend_label.add_css_class("mode-control-label");

    let backend_combo = ComboBoxText::new();
    let model_label = Label::new(Some("Model"));
    model_label.set_xalign(0.0);
    model_label.add_css_class("mode-control-label");
    let model_combo = ComboBoxText::new();
    populate_provider_combo(
        &backend_combo,
        &runtime.borrow().providers,
        &state.borrow().app_state.active_provider,
    );
    populate_model_combo(
        &model_combo,
        &runtime.borrow(),
        &state.borrow().app_state.active_provider,
        &state.borrow().app_state.active_model,
    );
    refresh_backend_banner(
        &title_label,
        &subtitle_label,
        &backend_combo,
        &model_combo,
        &state.borrow(),
        &runtime.borrow(),
    );

    banner.append(&title_label);
    banner.append(&subtitle_label);

    let backend_group = Box::new(Orientation::Vertical, 4);
    backend_group.append(&backend_label);
    backend_group.append(&backend_combo);
    backend_group.append(&model_label);
    backend_group.append(&model_combo);

    controls.append(&backend_group);
    banner.append(&controls);

    let title_label_clone = title_label.clone();
    let subtitle_label_clone = subtitle_label.clone();
    let backend_combo_clone = backend_combo.clone();
    let model_combo_clone = model_combo.clone();
    let provider_combo_syncing = Rc::new(RefCell::new(false));
    let model_combo_syncing = Rc::new(RefCell::new(false));
    let banner_handles = Rc::new(BackendBannerHandles {
        title_label: title_label.clone(),
        subtitle_label: subtitle_label.clone(),
        backend_combo: backend_combo.clone(),
        model_combo: model_combo.clone(),
        provider_combo_syncing: provider_combo_syncing.clone(),
        model_combo_syncing: model_combo_syncing.clone(),
    });
    let state_clone = state.clone();
    let log_buffer_clone = log_buffer.clone();
    let runtime_clone = runtime.clone();
    let stack_clone = stack.clone();

    let provider_combo_syncing_for_provider = provider_combo_syncing.clone();
    let model_combo_syncing_for_provider = model_combo_syncing.clone();
    backend_combo.connect_changed(move |combo| {
        if *provider_combo_syncing_for_provider.borrow() {
            return;
        }
        if let Some(selected) = combo.active_id() {
            {
                let mut state = state_clone.borrow_mut();
                state.app_state.active_provider = selected.to_string();
                let runtime = runtime_clone.borrow();
                sync_active_model_with_provider(&mut state, &runtime);
                persist_state(&state);
            }
            let state = state_clone.borrow();
            let runtime = runtime_clone.borrow();
            *model_combo_syncing_for_provider.borrow_mut() = true;
            *provider_combo_syncing_for_provider.borrow_mut() = true;
            populate_model_combo(
                &model_combo_clone,
                &runtime,
                &state.app_state.active_provider,
                &state.app_state.active_model,
            );
            refresh_backend_banner(
                &title_label_clone,
                &subtitle_label_clone,
                &backend_combo_clone,
                &model_combo_clone,
                &state,
                &runtime,
            );
            *provider_combo_syncing_for_provider.borrow_mut() = false;
            *model_combo_syncing_for_provider.borrow_mut() = false;
            append_log(
                &log_buffer_clone,
                &format!(
                    "[mode] provider switched to {}",
                    state.app_state.active_provider
                ),
            );
        }
    });

    {
        let state_clone = state.clone();
        let runtime_clone = runtime.clone();
        let title_label_clone = title_label.clone();
        let subtitle_label_clone = subtitle_label.clone();
        let backend_combo_clone = backend_combo.clone();
        let model_combo_clone = model_combo.clone();
        let log_buffer_clone = log_buffer.clone();
        let provider_combo_syncing = provider_combo_syncing.clone();
        let model_combo_syncing = model_combo_syncing.clone();
        model_combo.connect_changed(move |combo| {
            if *model_combo_syncing.borrow() {
                return;
            }
            if let Some(selected) = combo.active_id() {
                let mut state = state_clone.borrow_mut();
                state.app_state.active_model = selected.to_string();
                persist_state(&state);
                *provider_combo_syncing.borrow_mut() = true;
                *model_combo_syncing.borrow_mut() = true;
                refresh_backend_banner(
                    &title_label_clone,
                    &subtitle_label_clone,
                    &backend_combo_clone,
                    &model_combo_clone,
                    &state,
                    &runtime_clone.borrow(),
                );
                *provider_combo_syncing.borrow_mut() = false;
                *model_combo_syncing.borrow_mut() = false;
                append_log(
                    &log_buffer_clone,
                    &format!("[mode] model switched to {}", state.app_state.active_model),
                );
            }
        });
    }

    let refresh_button = Button::with_label("Refresh");
    refresh_button.add_css_class("sidebar-button");
    let refresh_log_buffer = log_buffer.clone();
    let refresh_providers_page = providers_page.clone();
    let refresh_quick_prompts_page = quick_prompts_page.clone();
    let refresh_composer_status = composer_status_label.clone();
    let refresh_banner_handles = banner_handles.clone();
    let refresh_state = state.clone();
    let refresh_runtime = runtime.clone();
    let refresh_stack = stack_clone.clone();
    refresh_button.connect_clicked(move |_| {
        refresh_ai_runtime_async(
            refresh_runtime.clone(),
            refresh_state.clone(),
            refresh_banner_handles.clone(),
            refresh_providers_page.clone(),
            refresh_quick_prompts_page.clone(),
            refresh_composer_status.clone(),
            refresh_log_buffer.clone(),
            refresh_stack.clone(),
            true,
            "manual refresh",
        );
    });
    controls.append(&refresh_button);

    (banner, banner_handles)
}

fn refresh_backend_banner(
    title_label: &Label,
    subtitle_label: &Label,
    backend_combo: &ComboBoxText,
    model_combo: &ComboBoxText,
    state: &PersistedState,
    runtime: &AiConsoleRuntime,
) {
    title_label.set_text("AI Console");
    if let Some(error) = runtime.load_error.as_ref() {
        subtitle_label.set_text(&format!("AI daemon query failed: {error}"));
    } else {
        let provider_count = runtime.providers.len();
        let active_provider = if state.app_state.active_provider.is_empty() {
            runtime
                .default_provider
                .as_deref()
                .unwrap_or("unknown")
                .to_string()
        } else {
            state.app_state.active_provider.clone()
        };
        let active_model = effective_model_label(state, runtime);
        let status = runtime
            .status
            .as_ref()
            .map(|status| {
                format!(
                    "active requests: {}, providers: {}",
                    status.active_requests, status.provider_count
                )
            })
            .unwrap_or_else(|| "daemon status unavailable".to_string());
        subtitle_label.set_text(&format!(
            "{provider_count} providers available. Active: {active_provider} / {active_model}. {status}"
        ));
    }

    populate_provider_combo(
        backend_combo,
        &runtime.providers,
        &state.app_state.active_provider,
    );
    populate_model_combo(
        model_combo,
        runtime,
        &state.app_state.active_provider,
        &state.app_state.active_model,
    );
}

fn populate_provider_combo(
    backend_combo: &ComboBoxText,
    providers: &[ProviderInfo],
    selected_provider: &str,
) {
    backend_combo.remove_all();
    if providers.is_empty() {
        backend_combo.append(Some("unavailable"), "No providers available");
        backend_combo.set_active_id(Some("unavailable"));
        return;
    }

    for provider in providers {
        backend_combo.append(Some(&provider.id), &provider_label(provider));
    }

    if providers
        .iter()
        .any(|provider| provider.id == selected_provider)
    {
        backend_combo.set_active_id(Some(selected_provider));
    } else if let Some(default_provider) = providers.first() {
        backend_combo.set_active_id(Some(&default_provider.id));
    }
}

fn populate_model_combo(
    model_combo: &ComboBoxText,
    runtime: &AiConsoleRuntime,
    selected_provider: &str,
    selected_model: &str,
) {
    model_combo.remove_all();
    let models = provider_models_for(runtime, selected_provider);
    if models.is_empty() {
        model_combo.append(Some("unavailable"), "No models listed");
        model_combo.set_active_id(Some("unavailable"));
        return;
    }

    for model in models {
        model_combo.append(Some(&model.id), &model_label(&model));
    }

    if !selected_model.is_empty() {
        model_combo.set_active_id(Some(selected_model));
        if model_combo.active_id().as_deref() == Some(selected_model) {
            return;
        }
    }

    if let Some(default_model) = runtime
        .providers
        .iter()
        .find(|provider| provider.id == selected_provider)
        .and_then(|provider| provider.default_model.clone())
        .filter(|default_model| {
            runtime
                .provider_models
                .get(selected_provider)
                .map(|models| models.iter().any(|model| model.id == *default_model))
                .unwrap_or(false)
        })
    {
        model_combo.set_active_id(Some(&default_model));
    } else if let Some(first_model) = provider_models_for(runtime, selected_provider).first() {
        model_combo.set_active_id(Some(&first_model.id));
    }
}

fn refresh_composer_status_label(
    label: &Label,
    state: &PersistedState,
    runtime: &AiConsoleRuntime,
) {
    let provider = if state.app_state.active_provider.is_empty() {
        runtime
            .default_provider
            .as_deref()
            .unwrap_or("unknown")
            .to_string()
    } else {
        state.app_state.active_provider.clone()
    };
    let model = effective_model_label(state, runtime);

    label.set_text(&format!("Composer backend: {provider} / {model}"));
}

fn provider_label(provider: &ProviderInfo) -> String {
    let model = provider.default_model.as_deref().unwrap_or("default model");
    format!("{} ({}, {})", provider.id, provider.kind, model)
}

fn build_providers_page(
    store: Rc<RefCell<PersistedState>>,
    runtime: Rc<RefCell<AiConsoleRuntime>>,
    quick_prompts_page: Rc<QuickPromptsPage>,
    composer_status_label: Label,
    log_buffer: TextBuffer,
) -> Rc<ProvidersPage> {
    let page = section_shell("Providers", "Registered AI backends exposed by the daemon");
    let summary_box = Box::new(Orientation::Vertical, 6);
    let list_box = Box::new(Orientation::Vertical, 6);
    summary_box.set_hexpand(true);
    summary_box.set_vexpand(true);
    list_box.set_hexpand(true);
    list_box.set_vexpand(true);
    list_box.set_width_request(360);

    let split = Paned::new(Orientation::Horizontal);
    split.add_css_class("split-pane");
    split.set_start_child(Some(&summary_box));
    let list_revealer = Revealer::new();
    list_revealer.set_child(Some(&list_box));
    list_revealer.set_reveal_child(false);
    split.set_end_child(Some(&list_revealer));
    split.set_position(740);
    split.set_wide_handle(true);
    split.set_vexpand(true);

    let list_toggle = Button::with_label("Show providers");
    list_toggle.add_css_class("sidebar-button");
    {
        let list_revealer = list_revealer.clone();
        let list_toggle_state = list_toggle.clone();
        list_toggle.connect_clicked(move |_| {
            let reveal = !list_revealer.reveals_child();
            list_revealer.set_reveal_child(reveal);
            list_toggle_state.set_label(if reveal {
                "Hide providers"
            } else {
                "Show providers"
            });
        });
    }
    summary_box.append(&list_toggle);
    page.append(&split);

    let view = Rc::new(ProvidersPage {
        page,
        summary_box,
        list_box,
        store,
        runtime,
        quick_prompts_page,
        composer_status_label,
        log_buffer,
    });
    refresh_providers_page_view(view.clone());
    view
}

fn build_quick_prompts_page() -> Rc<QuickPromptsPage> {
    let page = section_shell(
        "Quick Prompts",
        "Real prompts that route through the AI daemon",
    );
    let activity_box = Box::new(Orientation::Vertical, 6);
    let detail_box = Box::new(Orientation::Vertical, 6);
    activity_box.set_hexpand(true);
    activity_box.set_vexpand(true);
    detail_box.set_hexpand(true);
    detail_box.set_vexpand(true);
    detail_box.set_width_request(360);

    let split = Paned::new(Orientation::Horizontal);
    split.add_css_class("split-pane");
    split.set_start_child(Some(&activity_box));
    let detail_revealer = Revealer::new();
    detail_revealer.set_child(Some(&detail_box));
    detail_revealer.set_reveal_child(false);
    split.set_end_child(Some(&detail_revealer));
    split.set_position(720);
    split.set_wide_handle(true);
    split.set_vexpand(true);

    let detail_toggle = Button::with_label("Show response");
    detail_toggle.add_css_class("sidebar-button");
    {
        let detail_revealer = detail_revealer.clone();
        let detail_toggle_state = detail_toggle.clone();
        detail_toggle.connect_clicked(move |_| {
            let reveal = !detail_revealer.reveals_child();
            detail_revealer.set_reveal_child(reveal);
            detail_toggle_state.set_label(if reveal {
                "Hide response"
            } else {
                "Show response"
            });
        });
    }
    activity_box.append(&detail_toggle);
    page.append(&split);

    let view = Rc::new(QuickPromptsPage {
        page,
        activity_box,
        detail_box,
        state: Rc::new(RefCell::new(PromptActivity::default())),
    });
    refresh_quick_prompts_page_view(view.clone());
    view
}

fn refresh_quick_prompts_page_view(view: Rc<QuickPromptsPage>) {
    clear_box(&view.activity_box);
    clear_box(&view.detail_box);

    let snapshot = view.state.borrow();
    view.activity_box.append(&info_card(&[
        format!(
            "Last prompt: {}",
            snapshot.last_label.as_deref().unwrap_or("none")
        ),
        format!(
            "Active backend: {} / {}",
            snapshot.active_provider.as_deref().unwrap_or("unknown"),
            snapshot.active_model.as_deref().unwrap_or("unknown")
        ),
        if snapshot.in_flight {
            "Status: waiting for daemon response".to_string()
        } else {
            "Status: idle".to_string()
        },
    ]));

    let request = snapshot
        .last_request
        .as_deref()
        .unwrap_or("No prompt has been sent yet.");
    view.detail_box
        .append(&info_card(&[format!("Last request:\n{request}")]));

    if let Some(response) = snapshot.last_response.as_ref() {
        view.detail_box
            .append(&info_card(&[format!("Last response:\n{response}")]));
    } else if let Some(error) = snapshot.last_error.as_ref() {
        view.detail_box
            .append(&info_card(&[format!("Last error:\n{error}")]));
    } else {
        view.detail_box
            .append(&info_card(&["No response yet.".to_string()]));
    }
}

fn sync_quick_prompts_backend(
    view: &Rc<QuickPromptsPage>,
    state: &PersistedState,
    runtime: &AiConsoleRuntime,
) {
    let mut snapshot = view.state.borrow_mut();
    snapshot.active_provider = if state.app_state.active_provider.is_empty() {
        runtime.default_provider.clone()
    } else {
        Some(state.app_state.active_provider.clone())
    };
    snapshot.active_model = effective_runtime_model(state, runtime);
    drop(snapshot);
    refresh_quick_prompts_page_view(view.clone());
}

fn refresh_providers_page_view(view: Rc<ProvidersPage>) {
    clear_box(&view.summary_box);
    clear_box(&view.list_box);

    let snapshot = view.store.borrow().clone();
    let runtime_snapshot = view.runtime.borrow().clone();

    view.summary_box.append(&info_card(&[
        format!("Active provider: {}", snapshot.app_state.active_provider),
        format!(
            "Active model: {}",
            effective_model_label(&snapshot, &runtime_snapshot)
        ),
        runtime_snapshot
            .status
            .as_ref()
            .map(|status| format!("Active requests: {}", status.active_requests))
            .unwrap_or_else(|| "Daemon status unavailable".to_string()),
    ]));

    if let Some(error) = runtime_snapshot.load_error.as_ref() {
        let status = StatusBanner::new("AI service unavailable");
        status.set(StateKind::ServiceUnavailable, "AI service unavailable");
        status.set_details(Some(error));
        view.summary_box.append(&status.widget());
    }

    if !runtime_snapshot.provider_model_errors.is_empty() {
        let errors = runtime_snapshot
            .provider_model_errors
            .iter()
            .map(|(provider, error)| format!("{provider}: {error}"))
            .collect::<Vec<_>>()
            .join("\n");
        view.summary_box
            .append(&info_card(&[format!("Model listing issues:\n{errors}")]));
    }

    for provider in runtime_snapshot.providers.iter() {
        let row = Box::new(Orientation::Vertical, 6);
        row.add_css_class("item-card");
        let models = provider_models_for(&runtime_snapshot, &provider.id);
        let model_text = if models.is_empty() {
            "Installed models: none listed".to_string()
        } else {
            format!(
                "Installed models: {}",
                models
                    .iter()
                    .map(model_label)
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };

        let title = Label::new(Some(&provider_label(provider)));
        title.set_xalign(0.0);
        title.add_css_class("item-title");

        let details = Label::new(Some(&format!(
            "Base URL: {}\n{}",
            provider.base_url.as_deref().unwrap_or("-"),
            model_text
        )));
        details.set_xalign(0.0);
        details.set_wrap(true);
        details.add_css_class("item-body");

        let button = Button::with_label(if provider.id == snapshot.app_state.active_provider {
            "Selected"
        } else {
            "Use provider"
        });
        button.add_css_class("sidebar-button");
        let provider_id = provider.id.clone();
        let store = view.store.clone();
        let runtime = view.runtime.clone();
        let quick_prompts_page = view.quick_prompts_page.clone();
        let composer_status_label = view.composer_status_label.clone();
        let log_buffer = view.log_buffer.clone();
        let view_for_refresh = view.clone();
        button.connect_clicked(move |_| {
            {
                let mut state = store.borrow_mut();
                state.app_state.active_provider = provider_id.clone();
                {
                    let runtime = runtime.borrow();
                    sync_active_model_with_provider(&mut state, &runtime);
                }
                persist_state(&state);
                append_log(
                    &log_buffer,
                    &format!("[provider] selected {}", state.app_state.active_provider),
                );
                sync_quick_prompts_backend(&quick_prompts_page, &state, &runtime.borrow());
                refresh_composer_status_label(&composer_status_label, &state, &runtime.borrow());
            }
            refresh_providers_page_view(view_for_refresh.clone());
        });

        row.append(&title);
        row.append(&details);
        row.append(&button);
        view.list_box.append(&row);
    }
}

fn tools_page(
    chat_view: Box,
    conversation_detail: Box,
    stack: gtk4::Stack,
    _composer: Box,
    entry: Entry,
    send_button: Button,
    active_nav: Rc<std::cell::RefCell<String>>,
    nav_buttons: Rc<std::cell::RefCell<Vec<Button>>>,
    store: Rc<RefCell<PersistedState>>,
    quick_prompts_page: Rc<QuickPromptsPage>,
    log_buffer: TextBuffer,
) -> Box {
    let page = quick_prompts_page.page.clone();
    page.append(&info_card(&[
        "These buttons prefill the composer with a real prompt.".to_string(),
        "They do not fabricate assistant output locally.".to_string(),
    ]));

    for (label, prompt) in [
        (
            "Summarize chat",
            "Summarize the current conversation and note the next action.",
        ),
        (
            "Draft reply",
            "Draft a concise reply to the current conversation.",
        ),
        (
            "Analyze provider",
            "Review the active AI provider and suggest whether it is suitable for this task.",
        ),
    ] {
        let button = action_button(label);
        let chat_view = chat_view.clone();
        let conversation_detail = conversation_detail.clone();
        let active_nav = active_nav.clone();
        let nav_buttons = nav_buttons.clone();
        let stack = stack.clone();
        let entry = entry.clone();
        let send_button = send_button.clone();
        let prompt_text = prompt.to_string();
        let store = store.clone();
        let log_buffer = log_buffer.clone();
        let quick_prompts_page = quick_prompts_page.clone();
        button.connect_clicked(move |_| {
            dispatch_chat_request_async(
                store.clone(),
                chat_view.clone(),
                conversation_detail.clone(),
                entry.clone(),
                send_button.clone(),
                log_buffer.clone(),
                prompt_text.clone(),
                label,
                Some(quick_prompts_page.clone()),
            );
            set_active_nav("New Chat", &active_nav, &nav_buttons);
            stack.set_visible_child_name("new-chat");
        });
        page.append(&button);
    }

    page.append(&info_card(&[
        "Desktop actions call the compositor or launch configured apps.".to_string(),
        "They are not local placeholders.".to_string(),
    ]));

    for (label, action_kind) in [
        ("Notify desktop", "notify"),
        ("Identify displays", "identify"),
        ("Launch terminal", "terminal"),
        ("Launch browser", "browser"),
        ("Open files", "files"),
    ] {
        let button = action_button(label);
        let log_buffer = log_buffer.clone();
        let action_kind = action_kind.to_string();
        button.connect_clicked(move |_| {
            let result = match action_kind.as_str() {
                "notify" => send_notification_request(&NotificationIpcRequest::Notify {
                    title: "FocalDesk AI Console".to_string(),
                    body: "Desktop action triggered from the AI console".to_string(),
                    timeout_ms: Some(3000),
                })
                .map_err(anyhow::Error::msg)
                .and_then(|response| match response {
                    NotificationIpcResponse::NotificationQueued { id } => {
                        Ok(format!("notification queued: {id}"))
                    }
                    NotificationIpcResponse::Ok => Ok("notification sent".to_string()),
                    NotificationIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
                    other => Err(anyhow::anyhow!(
                        "unexpected notification response: {other:?}"
                    )),
                }),
                "identify" => send_desktop_request(&IpcRequest::IdentifyDisplays)
                    .map_err(anyhow::Error::msg)
                    .and_then(|response| match response {
                        IpcResponse::Ok => Ok("display identification requested".to_string()),
                        IpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
                        other => Err(anyhow::anyhow!("unexpected desktop response: {other:?}")),
                    }),
                "terminal" => {
                    launch_configured_app_async(log_buffer.clone(), "terminal", |settings| {
                        settings.apps.terminal.clone()
                    })
                }
                "browser" => {
                    launch_configured_app_async(log_buffer.clone(), "browser", |settings| {
                        settings.apps.browser.clone()
                    })
                }
                "files" => {
                    launch_configured_app_async(log_buffer.clone(), "file manager", |settings| {
                        settings.apps.file_manager.clone()
                    })
                }
                _ => Err(anyhow::anyhow!("unknown desktop action")),
            };

            match result {
                Ok(message) => append_log(&log_buffer, &format!("[action] {message}")),
                Err(err) => append_log(&log_buffer, &format!("[action] failed: {err}")),
            }
        });
        page.append(&button);
    }

    page
}

fn debug_page(log_buffer: TextBuffer) -> Box {
    let page = section_shell("Log/Debug", "Live console output");
    page.append(&info_card(&[
        "This log reflects nav clicks, provider changes, and conversation edits.".to_string(),
        "It is useful for tracing the real AI backend path end to end.".to_string(),
    ]));
    let log = TextView::new();
    log.set_editable(false);
    log.set_cursor_visible(false);
    log.set_monospace(true);
    log.set_buffer(Some(&log_buffer));

    let scroll = ScrolledWindow::builder()
        .child(&log)
        .vexpand(true)
        .hexpand(true)
        .build();
    page.append(&scroll);
    page
}

fn append_log(buffer: &TextBuffer, line: &str) {
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, &format!("{line}\n"));
}

fn send_chat_request(request: ChatRequest) -> anyhow::Result<String> {
    match send_ai_request(&AiIpcRequest::Chat { request })? {
        focaldesk_ai::AiIpcResponse::Chat { response } => Ok(response.content),
        focaldesk_ai::AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

const MEMORY_RECALL_TOP_K: usize = 5;

fn send_remember_request(text: String, metadata: serde_json::Value) -> anyhow::Result<MemoryId> {
    match send_ai_request(&AiIpcRequest::Remember { text, metadata })? {
        AiIpcResponse::Remembered { id } => Ok(id),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn send_recall_request(query: String, top_k: usize) -> anyhow::Result<Vec<SearchHit>> {
    match send_ai_request(&AiIpcRequest::Recall { query, top_k })? {
        AiIpcResponse::Recalled { hits } => Ok(hits),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn launch_configured_app(
    selector: impl FnOnce(&focaldesk_settings_core::Settings) -> String,
) -> anyhow::Result<String> {
    let settings = load_settings();
    let command = selector(&settings);
    Command::new(&command)
        .spawn()
        .with_context(|| format!("failed to launch {command}"))?;
    Ok(command)
}

fn launch_configured_app_async(
    log_buffer: TextBuffer,
    label: &'static str,
    selector: impl FnOnce(&focaldesk_settings_core::Settings) -> String + Send + 'static,
) -> anyhow::Result<String> {
    let (tx, rx) = mpsc::channel();
    append_log(
        &log_buffer,
        &format!("[action] queueing {label} launch on background thread"),
    );

    thread::spawn(move || {
        let result = launch_configured_app(selector);
        let _ = tx.send(result);
    });

    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(command)) => {
            append_log(
                &log_buffer,
                &format!("[action] launched {label}: {command}"),
            );
            glib::ControlFlow::Break
        }
        Ok(Err(err)) => {
            append_log(
                &log_buffer,
                &format!("[action] failed to launch {label}: {err}"),
            );
            glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            append_log(
                &log_buffer,
                &format!("[action] failed to launch {label}: background task disconnected"),
            );
            glib::ControlFlow::Break
        }
    });

    Ok(format!("queued {label} launch"))
}

fn refresh_ai_runtime_async(
    runtime: Rc<RefCell<AiConsoleRuntime>>,
    state: Rc<RefCell<PersistedState>>,
    banner: Rc<BackendBannerHandles>,
    providers_page: Rc<ProvidersPage>,
    quick_prompts_page: Rc<QuickPromptsPage>,
    composer_status_label: Label,
    log_buffer: TextBuffer,
    stack: gtk4::Stack,
    show_providers_after_load: bool,
    label: &'static str,
) {
    let (tx, rx) = mpsc::channel();
    let started_at = Instant::now();
    append_log(
        &log_buffer,
        &format!("[ai] queueing runtime {label} refresh on background thread"),
    );

    thread::spawn(move || {
        let result = load_ai_runtime();
        let _ = tx.send(result);
    });

    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(runtime_snapshot) => {
            {
                let mut runtime_state = runtime.borrow_mut();
                *runtime_state = runtime_snapshot;
            }

            {
                let runtime_snapshot = runtime.borrow();
                let mut state_snapshot = state.borrow_mut();
                normalize_state_with_runtime(&mut state_snapshot, &runtime_snapshot);
                persist_state(&state_snapshot);
            }

            let state_snapshot = state.borrow();
            let runtime_snapshot = runtime.borrow();
            banner.refresh(&state_snapshot, &runtime_snapshot);
            refresh_composer_status_label(
                &composer_status_label,
                &state_snapshot,
                &runtime_snapshot,
            );
            sync_quick_prompts_backend(&quick_prompts_page, &state_snapshot, &runtime_snapshot);
            refresh_providers_page_view(providers_page.clone());
            append_log(
                &log_buffer,
                &format!(
                    "[ai] runtime {label} refreshed in {} ms",
                    started_at.elapsed().as_millis()
                ),
            );

            if show_providers_after_load {
                stack.set_visible_child_name("providers");
            }

            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            append_log(
                &log_buffer,
                &format!(
                    "[ai] runtime {label} refresh failed after {} ms: background task disconnected",
                    started_at.elapsed().as_millis()
                ),
            );
            ControlFlow::Break
        }
    });
}

fn dispatch_chat_request_async(
    state: Rc<RefCell<PersistedState>>,
    chat_view: Box,
    conversation_detail: Box,
    entry: Entry,
    send_button: Button,
    log_buffer: TextBuffer,
    prompt_text: String,
    source_label: &str,
    quick_prompts_page: Option<Rc<QuickPromptsPage>>,
) {
    if prompt_text.trim().is_empty() {
        return;
    }

    let source_label = source_label.to_string();
    let active_idx;
    let provider;
    let model;
    let request;
    {
        let mut store = state.borrow_mut();
        active_idx = store
            .app_state
            .active_conversation
            .min(store.conversations.len().saturating_sub(1));
        provider = store.app_state.active_provider.clone();
        model = store.app_state.active_model.clone();
        request = build_chat_request(&store, active_idx, &prompt_text);

        if let Some(conversation) = store.conversations.get_mut(active_idx) {
            conversation.messages.push(format!("User: {}", prompt_text));
            conversation
                .messages
                .push("AI: [pending response from daemon]".to_string());
            conversation.summary = format!("Waiting for {} response", source_label);
        }

        persist_state(&store);
        render_active_conversation(&chat_view, &store);
        if let Some(conversation) = store.conversations.get(active_idx).cloned() {
            render_conversation_panel(&conversation_detail, &conversation, "Active conversation");
        }
    }

    if let Some(view) = quick_prompts_page.as_ref() {
        {
            let mut prompt_state = view.state.borrow_mut();
            prompt_state.last_label = Some(source_label.to_string());
            prompt_state.last_request = Some(prompt_text.clone());
            prompt_state.last_response = None;
            prompt_state.last_error = None;
            prompt_state.active_provider = Some(provider.clone());
            prompt_state.active_model = Some(model.clone());
            prompt_state.in_flight = true;
        }
        refresh_quick_prompts_page_view(view.clone());
    }

    send_button.set_sensitive(false);
    entry.set_text("");
    append_log(
        &log_buffer,
        &format!(
            "[chat] {} request sent via provider {} ({})",
            source_label, provider, model
        ),
    );

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let result = send_chat_request(request);
        let _ = tx.send(result);
    });

    let state_for_result = state.clone();
    let chat_view_for_result = chat_view.clone();
    let detail_for_result = conversation_detail.clone();
    let entry_for_result = entry.clone();
    let send_button_for_result = send_button.clone();
    let log_buffer_for_result = log_buffer.clone();
    let provider_for_result = provider;
    let model_for_result = model;
    let source_label_for_result = source_label.clone();
    let quick_prompts_page_for_result = quick_prompts_page.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(reply)) => {
            let mut store = state_for_result.borrow_mut();
            apply_ai_reply(&mut store, active_idx, &reply, "Recently updated");
            persist_state(&store);
            render_active_conversation(&chat_view_for_result, &store);
            if let Some(conversation) = store.conversations.get(active_idx).cloned() {
                render_conversation_panel(&detail_for_result, &conversation, "Active conversation");
            }
            append_log(
                &log_buffer_for_result,
                &format!(
                    "[chat] {} response received via provider {} ({})",
                    source_label_for_result, provider_for_result, model_for_result
                ),
            );
            if let Some(view) = quick_prompts_page_for_result.as_ref() {
                {
                    let mut prompt_state = view.state.borrow_mut();
                    prompt_state.last_response = Some(reply.clone());
                    prompt_state.last_error = None;
                    prompt_state.in_flight = false;
                    prompt_state.active_provider = Some(provider_for_result.clone());
                    prompt_state.active_model = Some(model_for_result.clone());
                }
                refresh_quick_prompts_page_view(view.clone());
            }
            send_button_for_result.set_sensitive(true);
            entry_for_result.set_text("");
            ControlFlow::Break
        }
        Ok(Err(err)) => {
            let error_message = format!("AI backend error: {err}");
            let mut store = state_for_result.borrow_mut();
            apply_ai_reply(&mut store, active_idx, &error_message, "Backend error");
            persist_state(&store);
            render_active_conversation(&chat_view_for_result, &store);
            if let Some(conversation) = store.conversations.get(active_idx).cloned() {
                render_conversation_panel(&detail_for_result, &conversation, "Active conversation");
            }
            append_log(&log_buffer_for_result, &format!("[chat] {error_message}"));
            if let Some(view) = quick_prompts_page_for_result.as_ref() {
                {
                    let mut prompt_state = view.state.borrow_mut();
                    prompt_state.last_response = None;
                    prompt_state.last_error = Some(error_message.clone());
                    prompt_state.in_flight = false;
                    prompt_state.active_provider = Some(provider_for_result.clone());
                    prompt_state.active_model = Some(model_for_result.clone());
                }
                refresh_quick_prompts_page_view(view.clone());
            }
            send_button_for_result.set_sensitive(true);
            entry_for_result.set_text("");
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            append_log(
                &log_buffer_for_result,
                "[chat] response channel disconnected before completion",
            );
            if let Some(view) = quick_prompts_page_for_result.as_ref() {
                {
                    let mut prompt_state = view.state.borrow_mut();
                    prompt_state.last_error =
                        Some("response channel disconnected before completion".to_string());
                    prompt_state.in_flight = false;
                    prompt_state.active_provider = Some(provider_for_result.clone());
                    prompt_state.active_model = Some(model_for_result.clone());
                }
                refresh_quick_prompts_page_view(view.clone());
            }
            send_button_for_result.set_sensitive(true);
            ControlFlow::Break
        }
    });
}

fn apply_ai_reply(store: &mut PersistedState, active_idx: usize, reply: &str, summary: &str) {
    if let Some(conversation) = store.conversations.get_mut(active_idx) {
        if let Some(last_message) = conversation.messages.last_mut() {
            if last_message == "AI: [pending response from daemon]" {
                *last_message = format!("AI: {reply}");
            } else {
                conversation.messages.push(format!("AI: {reply}"));
            }
        } else {
            conversation.messages.push(format!("AI: {reply}"));
        }
        conversation.summary = summary.to_string();
    }
}

fn build_chat_request(
    store: &PersistedState,
    conversation_idx: usize,
    prompt: &str,
) -> ChatRequest {
    let mut messages = vec![ChatMessage::system(
        "You are the FocalDesk AI Console. Keep responses concise. Respond in English unless the user requests another language.",
    )];

    if let Some(conversation) = store.conversations.get(conversation_idx) {
        messages.push(ChatMessage::system(format!(
            "Conversation: {}",
            conversation.title
        )));

        let history_start = conversation.messages.len().saturating_sub(8);
        for message in &conversation.messages[history_start..] {
            if let Some(user_content) = message.strip_prefix("User: ") {
                messages.push(ChatMessage::user(user_content.to_string()));
            } else if let Some(ai_content) = message.strip_prefix("AI: ") {
                messages.push(ChatMessage::assistant(ai_content.to_string()));
            } else if message.starts_with("AI (") {
                messages.push(ChatMessage::assistant(message.clone()));
            }
        }
    }

    // The new prompt must remain the final turn so providers see a coherent,
    // chronological conversation ending with the request they should answer.
    messages.push(ChatMessage::user(prompt.to_string()));

    ChatRequest {
        provider: if store.app_state.active_provider.is_empty() {
            None
        } else {
            Some(store.app_state.active_provider.clone())
        },
        model: effective_request_model(store),
        messages,
        temperature: None,
        max_tokens: None,
        use_memory: false,
    }
}

fn create_new_conversation(
    store: &Rc<RefCell<PersistedState>>,
    chat_view: &Box,
    conversation_detail: &Box,
    log_buffer: &TextBuffer,
) {
    let mut state = store.borrow_mut();
    if let Some((index, conversation)) = state
        .conversations
        .iter()
        .enumerate()
        .rev()
        .find(|(_, conversation)| is_placeholder_conversation(conversation))
        .map(|(index, conversation)| (index, conversation.clone()))
    {
        let conversation = conversation.clone();
        state.app_state.active_conversation = index;
        persist_state(&state);
        render_active_conversation(chat_view, &state);
        render_conversation_panel(conversation_detail, &conversation, "Active conversation");
        append_log(
            log_buffer,
            &format!("[chat] reused empty conversation {}", index + 1),
        );
        return;
    }

    let next_number = state.conversations.len() + 1;
    state.conversations.push(Conversation {
        title: "New Chat".to_string(),
        summary: "Empty thread".to_string(),
        messages: Vec::new(),
    });
    state.app_state.active_conversation = state.conversations.len().saturating_sub(1);
    persist_state(&state);
    render_active_conversation(chat_view, &state);
    if let Some(conversation) = state.conversations.last().cloned() {
        render_conversation_panel(conversation_detail, &conversation, "Active conversation");
    }
    append_log(
        log_buffer,
        &format!("[chat] created conversation {next_number}"),
    );
}

fn load_active_conversation(chat_view: &Box, conversations: &[Conversation], app_state: &AppState) {
    if let Some(conversation) = conversations
        .get(app_state.active_conversation)
        .or_else(|| conversations.first())
    {
        load_conversation(chat_view, conversation);
    }
}

fn render_active_conversation(chat_view: &Box, store: &PersistedState) {
    if store.conversations.is_empty() {
        clear_box(chat_view);
        let empty = StateView::new(
            StateKind::Empty,
            "No conversation selected",
            "Start a new chat or open a saved conversation.",
        );
        chat_view.append(&empty.widget());
    } else {
        load_active_conversation(chat_view, &store.conversations, &store.app_state);
    }
}

fn clear_box(container: &Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn conversations_page(
    store: Rc<RefCell<PersistedState>>,
    chat_view: Box,
    detail_panel: Box,
    stack: gtk4::Stack,
    _composer: Box,
    active_nav: Rc<std::cell::RefCell<String>>,
    nav_buttons: Rc<std::cell::RefCell<Vec<Button>>>,
    log_buffer: TextBuffer,
) -> Box {
    let page = section_shell("Conversations", "Recent chats and saved threads");
    let snapshot = store.borrow();

    let overview = info_card(&[
        format!("{} conversations stored", snapshot.conversations.len()),
        format!(
            "Active thread: {}",
            snapshot
                .conversations
                .get(snapshot.app_state.active_conversation)
                .map(|conversation| conversation.title.as_str())
                .unwrap_or("none")
        ),
    ]);
    page.append(&overview);

    let list_column = Box::new(Orientation::Vertical, 8);
    list_column.set_hexpand(true);
    list_column.set_vexpand(true);
    let list_header = Box::new(Orientation::Horizontal, 8);
    let list_label = Label::new(Some("Conversation list"));
    list_label.set_xalign(0.0);
    list_label.add_css_class("pane-heading");
    list_label.set_hexpand(true);
    let detail_toggle = Button::with_label("Show details");
    detail_toggle.add_css_class("sidebar-button");
    list_header.append(&list_label);
    list_header.append(&detail_toggle);
    list_column.append(&list_header);

    let detail_column = Box::new(Orientation::Vertical, 8);
    detail_column.set_hexpand(true);
    detail_column.set_vexpand(true);
    detail_panel.set_width_request(320);
    let detail_label = Label::new(Some("Active conversation"));
    detail_label.set_xalign(0.0);
    detail_label.add_css_class("pane-heading");
    detail_column.append(&detail_label);

    let conversations = snapshot.conversations.clone();
    if let Some(active) = conversations
        .get(snapshot.app_state.active_conversation)
        .or_else(|| conversations.first())
    {
        render_conversation_panel(&detail_panel, active, "Active conversation");
    }
    let detail_scroll = ScrolledWindow::builder()
        .child(&detail_panel)
        .vexpand(true)
        .hexpand(true)
        .build();
    detail_scroll.add_css_class("pane-scroll");
    detail_column.append(&detail_scroll);

    let detail_revealer = Revealer::new();
    detail_revealer.set_child(Some(&detail_column));
    detail_revealer.set_reveal_child(false);

    {
        let detail_revealer = detail_revealer.clone();
        let detail_toggle_state = detail_toggle.clone();
        detail_toggle.clone().connect_clicked(move |_| {
            let reveal = !detail_revealer.reveals_child();
            detail_revealer.set_reveal_child(reveal);
            detail_toggle_state.set_label(if reveal {
                "Hide details"
            } else {
                "Show details"
            });
        });
    }

    for (index, conversation) in conversations.iter().enumerate() {
        let row = Box::new(Orientation::Vertical, 6);
        row.add_css_class("item-card");

        let header = Box::new(Orientation::Vertical, 2);
        let title_label = Label::new(Some(&conversation.title));
        title_label.set_xalign(0.0);
        title_label.add_css_class("conversation-preview-title");

        let summary_label = Label::new(Some(&conversation.summary));
        summary_label.set_xalign(0.0);
        summary_label.set_wrap(true);
        summary_label.add_css_class("conversation-preview-summary");

        header.append(&title_label);
        header.append(&summary_label);
        row.append(&header);

        let title_entry = Entry::builder()
            .text(&conversation.title)
            .placeholder_text("Conversation title")
            .hexpand(true)
            .build();
        let summary_entry = Entry::builder()
            .text(&conversation.summary)
            .placeholder_text("Conversation summary")
            .hexpand(true)
            .build();
        let load_button = Button::with_label("Load");
        load_button.add_css_class("sidebar-button");

        let controls = Box::new(Orientation::Horizontal, 8);
        controls.append(&load_button);
        row.append(&controls);
        row.append(&title_entry);
        row.append(&summary_entry);

        let chat_view = chat_view.clone();
        let active_nav = active_nav.clone();
        let nav_buttons = nav_buttons.clone();
        let stack = stack.clone();
        let load_store = store.clone();
        let load_log_buffer = log_buffer.clone();
        let detail_panel = detail_panel.clone();
        load_button.connect_clicked(move |_| {
            if let Some(conversation) = load_store.borrow().conversations.get(index).cloned() {
                load_conversation(&chat_view, &conversation);
                render_conversation_panel(&detail_panel, &conversation, "Active conversation");
            }
            {
                let mut state = load_store.borrow_mut();
                state.app_state.active_conversation = index;
                persist_state(&state);
            }
            append_log(
                &load_log_buffer,
                &format!("[chat] loaded conversation {}", index + 1),
            );
            set_active_nav("Conversations", &active_nav, &nav_buttons);
            stack.set_visible_child_name("conversations");
        });

        let title_store = store.clone();
        let title_log_buffer = log_buffer.clone();
        let title_label = title_label.clone();
        title_entry.connect_changed(move |entry| {
            let mut state = title_store.borrow_mut();
            if let Some(conversation) = state.conversations.get_mut(index) {
                let new_title = entry.text().to_string();
                conversation.title = new_title.clone();
                title_label.set_text(&new_title);
                persist_state(&state);
                append_log(
                    &title_log_buffer,
                    &format!("[conversation] renamed thread {}", index + 1),
                );
            }
        });

        let summary_store = store.clone();
        let summary_log_buffer = log_buffer.clone();
        let summary_label = summary_label.clone();
        summary_entry.connect_changed(move |entry| {
            let mut state = summary_store.borrow_mut();
            if let Some(conversation) = state.conversations.get_mut(index) {
                let new_summary = entry.text().to_string();
                conversation.summary = new_summary.clone();
                summary_label.set_text(&new_summary);
                persist_state(&state);
                append_log(
                    &summary_log_buffer,
                    &format!("[conversation] updated summary {}", index + 1),
                );
            }
        });

        list_column.append(&row);
    }

    let list_scroll = ScrolledWindow::builder()
        .child(&list_column)
        .vexpand(true)
        .hexpand(true)
        .build();
    list_scroll.add_css_class("pane-scroll");

    let split = Paned::new(Orientation::Horizontal);
    split.add_css_class("split-pane");
    split.set_start_child(Some(&list_scroll));
    split.set_end_child(Some(&detail_revealer));
    split.set_position(920);
    split.set_wide_handle(true);
    split.set_vexpand(true);
    page.append(&split);

    page
}

fn memory_page(store: Rc<RefCell<PersistedState>>, log_buffer: TextBuffer) -> Box {
    let page = section_shell("Memory", "Pinned facts and working notes");
    let snapshot = store.borrow();
    page.append(&info_card(&[
        format!("{} notes pinned", snapshot.app_state.memory_notes.len()),
        "Add short facts here; they are persisted locally and sent to the AI memory store for recall.".to_string(),
    ]));
    drop(snapshot);

    let notes_box = Box::new(Orientation::Vertical, 6);
    {
        let snapshot = store.borrow();
        for note in snapshot.app_state.memory_notes.clone() {
            notes_box.append(&note_card(&note));
        }
    }
    page.append(&notes_box);

    let entry = Entry::builder()
        .placeholder_text("Add memory note")
        .hexpand(true)
        .build();
    let button = Button::with_label("Add note");
    {
        let store = store.clone();
        let notes_box = notes_box.clone();
        let entry_clone = entry.clone();
        let log_buffer = log_buffer.clone();
        button.connect_clicked(move |_| {
            let text = entry_clone.text().to_string();
            if text.trim().is_empty() {
                return;
            }
            {
                let mut state = store.borrow_mut();
                state.app_state.memory_notes.push(text.clone());
                persist_state(&state);
            }
            notes_box.append(&note_card(&text));
            append_log(&log_buffer, "[memory] added a note");
            entry_clone.set_text("");

            let (tx, rx) = mpsc::channel();
            let remember_text = text.clone();
            thread::spawn(move || {
                let result = send_remember_request(
                    remember_text,
                    serde_json::json!({ "source": "ai-console" }),
                );
                let _ = tx.send(result);
            });

            let log_buffer_for_result = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(id)) => {
                    append_log(
                        &log_buffer_for_result,
                        &format!("[memory] stored note in AI memory store (id {id})"),
                    );
                    ControlFlow::Break
                }
                Ok(Err(err)) => {
                    append_log(
                        &log_buffer_for_result,
                        &format!("[memory] AI memory store unavailable: {err}"),
                    );
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    append_log(
                        &log_buffer_for_result,
                        "[memory] remember channel disconnected before completion",
                    );
                    ControlFlow::Break
                }
            });
        });
    }

    let row = Box::new(Orientation::Horizontal, 8);
    row.append(&entry);
    row.append(&button);
    page.append(&row);

    let recall_heading = Label::new(Some("Search memory"));
    recall_heading.set_xalign(0.0);
    recall_heading.add_css_class("pane-heading");
    page.append(&recall_heading);

    let recall_entry = Entry::builder()
        .placeholder_text("Search memory (e.g. \"garage code\")")
        .hexpand(true)
        .build();
    let recall_button = Button::with_label("Recall");
    let recall_results = Box::new(Orientation::Vertical, 6);

    {
        let recall_entry_clone = recall_entry.clone();
        let recall_results = recall_results.clone();
        let recall_button_clone = recall_button.clone();
        let log_buffer = log_buffer.clone();
        recall_button.connect_clicked(move |_| {
            let query = recall_entry_clone.text().to_string();
            if query.trim().is_empty() {
                return;
            }

            clear_box(&recall_results);
            recall_results.append(&note_card("Searching..."));
            recall_button_clone.set_sensitive(false);
            append_log(&log_buffer, &format!("[memory] recall query: {query}"));

            let (tx, rx) = mpsc::channel();
            let recall_query = query.clone();
            thread::spawn(move || {
                let result = send_recall_request(recall_query, MEMORY_RECALL_TOP_K);
                let _ = tx.send(result);
            });

            let recall_results_for_result = recall_results.clone();
            let recall_button_for_result = recall_button_clone.clone();
            let log_buffer_for_result = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(hits)) => {
                    clear_box(&recall_results_for_result);
                    if hits.is_empty() {
                        recall_results_for_result.append(&note_card("No matching memories found."));
                    } else {
                        for hit in &hits {
                            recall_results_for_result.append(&recall_hit_card(hit));
                        }
                    }
                    append_log(
                        &log_buffer_for_result,
                        &format!("[memory] recall returned {} hit(s)", hits.len()),
                    );
                    recall_button_for_result.set_sensitive(true);
                    ControlFlow::Break
                }
                Ok(Err(err)) => {
                    clear_box(&recall_results_for_result);
                    recall_results_for_result.append(&note_card(&format!("Recall failed: {err}")));
                    append_log(
                        &log_buffer_for_result,
                        &format!("[memory] recall failed: {err}"),
                    );
                    recall_button_for_result.set_sensitive(true);
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    clear_box(&recall_results_for_result);
                    recall_results_for_result
                        .append(&note_card("Recall channel disconnected before completion."));
                    recall_button_for_result.set_sensitive(true);
                    ControlFlow::Break
                }
            });
        });
    }

    let recall_row = Box::new(Orientation::Horizontal, 8);
    recall_row.append(&recall_entry);
    recall_row.append(&recall_button);
    page.append(&recall_row);
    page.append(&recall_results);

    page
}

fn settings_page(store: Rc<RefCell<PersistedState>>, log_buffer: TextBuffer) -> Box {
    let page = section_shell("Settings", "Console preferences");
    let snapshot = store.borrow().app_state.clone();
    page.append(&info_card(&[
        format!("Compact sidebar: {}", snapshot.compact_sidebar),
        format!("Show timestamps: {}", snapshot.show_timestamps),
        format!("Auto-scroll chat: {}", snapshot.auto_scroll),
        format!("Verbose output: {}", snapshot.verbose_output),
    ]));

    for (label, active) in [
        ("Compact sidebar", snapshot.compact_sidebar),
        ("Show timestamps", snapshot.show_timestamps),
        ("Auto-scroll chat", snapshot.auto_scroll),
        ("Verbose tool output", snapshot.verbose_output),
    ] {
        page.append(&toggle_row(
            label,
            active,
            store.clone(),
            log_buffer.clone(),
        ));
    }

    page
}

fn section_shell(title: &str, subtitle: &str) -> Box {
    let page = Box::new(Orientation::Vertical, 10);
    page.add_css_class("panel-page");

    let title_label = Label::new(Some(title));
    title_label.set_xalign(0.0);
    title_label.add_css_class("panel-title");

    let body = Label::new(Some(subtitle));
    body.set_xalign(0.0);
    body.set_wrap(true);
    body.add_css_class("panel-body");

    page.append(&title_label);
    page.append(&body);
    page
}

fn action_button(label: &str) -> Button {
    let button = Button::with_label(label);
    button.add_css_class("sidebar-button");
    button
}

fn note_card(text: &str) -> Box {
    let card = Box::new(Orientation::Vertical, 4);
    card.add_css_class("item-card");

    let label = Label::new(Some(text));
    label.set_xalign(0.0);
    label.set_wrap(true);
    label.add_css_class("item-body");

    card.append(&label);
    card
}

fn recall_hit_card(hit: &SearchHit) -> Box {
    let card = Box::new(Orientation::Vertical, 4);
    card.add_css_class("item-card");

    let text_label = Label::new(Some(&hit.record.text));
    text_label.set_xalign(0.0);
    text_label.set_wrap(true);
    text_label.add_css_class("item-body");
    card.append(&text_label);

    let meta_label = Label::new(Some(&format!("distance {:.3}", hit.distance)));
    meta_label.set_xalign(0.0);
    meta_label.add_css_class("item-meta");
    card.append(&meta_label);

    card
}

fn info_card(lines: &[String]) -> Box {
    let card = Box::new(Orientation::Vertical, 4);
    card.add_css_class("item-card");
    card.add_css_class("info-card");

    for line in lines {
        let label = Label::new(Some(line));
        label.set_xalign(0.0);
        label.set_wrap(true);
        label.add_css_class("item-body");
        card.append(&label);
    }

    card
}

fn toggle_row(
    label: &str,
    active: bool,
    store: Rc<RefCell<PersistedState>>,
    log_buffer: TextBuffer,
) -> Box {
    let row = Box::new(Orientation::Horizontal, 10);
    row.add_css_class("item-card");

    let text = Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_hexpand(true);

    let toggle = Switch::new();
    toggle.set_active(active);
    let key = label.to_string();
    let log_buffer = log_buffer.clone();
    toggle.connect_active_notify(move |s| {
        let mut state = store.borrow_mut();
        let value = s.is_active();
        match key.as_str() {
            "Compact sidebar" => state.app_state.compact_sidebar = value,
            "Show timestamps" => state.app_state.show_timestamps = value,
            "Auto-scroll chat" => state.app_state.auto_scroll = value,
            "Verbose tool output" => state.app_state.verbose_output = value,
            _ => {}
        }
        persist_state(&state);
        append_log(&log_buffer, &format!("[settings] {} => {}", key, value));
    });

    row.append(&text);
    row.append(&toggle);
    row
}

fn set_active_nav(
    label: &str,
    active_nav: &Rc<std::cell::RefCell<String>>,
    nav_buttons: &Rc<std::cell::RefCell<Vec<Button>>>,
) {
    if active_nav.borrow().as_str() == label {
        return;
    }

    for existing in nav_buttons.borrow().iter() {
        existing.remove_css_class("sidebar-button-active");
    }

    *active_nav.borrow_mut() = label.to_string();

    if let Some(active) = nav_buttons
        .borrow()
        .iter()
        .find(|b| b.label().map(|s| s == label).unwrap_or(false))
    {
        active.add_css_class("sidebar-button-active");
    }
}

const AI_CONSOLE_CSS: &str = r#"
        .ai-root {
            padding: 14px;
            background: @fd_app_bg;
            color: @fd_app_text;
        }

        .ai-sidebar {
            padding: 12px;
            border-radius: 18px;
            background: @fd_app_surface;
            border: 1px solid @fd_app_border;
        }

        .sidebar-button {
            border-radius: 12px;
            padding: 10px;
            background: @fd_app_surface_raised;
            color: @fd_app_text;
            border: 1px solid @fd_app_border;
        }

        .sidebar-button:hover {
            background: @fd_app_surface_hover;
        }

        .sidebar-button-active {
            background: @fd_app_accent_muted;
            color: #ffffff;
            border-color: @fd_app_accent_bright;
        }

        .ai-main {
            padding: 12px;
            border-radius: 18px;
            background: @fd_app_bg;
        }

        .mode-banner {
            padding: 12px;
            margin-bottom: 6px;
            border-radius: 16px;
            background: linear-gradient(90deg, @fd_app_accent_muted 0%, @fd_app_surface 100%);
            border: 1px solid @fd_app_accent;
        }

        .mode-banner-button {
            padding: 0;
            background: transparent;
            border: none;
        }

        .mode-banner-title {
            font-size: 1.0em;
            font-weight: 700;
            color: @fd_app_text;
        }

        .mode-banner-body {
            color: @fd_app_text_dim;
            font-size: 0.92em;
        }

        .chat-list {
            padding: 12px;
        }

        .chat-card {
            padding: 12px;
            border-radius: 14px;
            color: @fd_app_text;
            border: 1px solid @fd_app_border;
        }

        .panel-page {
            padding: 18px;
            border-radius: 16px;
            background: @fd_app_surface;
            border: 1px solid @fd_app_border;
        }

        .split-pane {
            spacing: 12px;
        }

        .transcript-pane {
            min-width: 0;
        }

        .detail-pane {
            min-width: 280px;
        }

        .pane-scroll {
            border-radius: 14px;
            border: 1px solid @fd_app_border;
            background: @fd_app_bg;
        }

        .pane-heading {
            font-size: 0.92em;
            font-weight: 700;
            color: @fd_app_text;
            padding-left: 2px;
        }

        .composer-status {
            min-width: 260px;
            color: @fd_app_text_dim;
            font-size: 0.85em;
        }

        .panel-title {
            font-size: 1.25em;
            font-weight: 700;
            color: @fd_app_text;
        }

        .panel-body {
            color: @fd_app_text_dim;
        }

        .item-card {
            padding: 12px;
            border-radius: 14px;
            background: @fd_app_surface_raised;
            border: 1px solid @fd_app_border;
        }

        .item-title {
            font-weight: 700;
            color: @fd_app_text;
        }

        .item-body {
            color: @fd_app_text;
        }

        .item-meta {
            color: @fd_app_text_dim;
            font-size: 0.78em;
        }

        .info-card {
            background: @fd_app_accent_muted;
            border: 1px solid @fd_app_accent;
        }

        .user-card {
            background: @fd_app_accent_muted;
        }

        .ai-card {
            background: @fd_app_surface;
        }

        .action-card {
            background: alpha(@fd_app_amber, 0.15);
            border: 1px solid @fd_app_amber;
        }

        .composer {
            padding: 10px;
            border-radius: 18px;
            background: @fd_app_surface;
            border: 1px solid @fd_app_border;
        }

        entry {
            border-radius: 14px;
            padding: 8px;
            background: @fd_app_input;
            color: @fd_app_text;
            border: 1px solid @fd_app_border;
        }

        button {
            border-radius: 12px;
        }

        combo,
        combobox,
        dropdown,
        switch {
            color: @fd_app_text;
        }
        "#;

fn load_css() {
    let provider = gtk4::CssProvider::new();
    let initial = active_theme_snapshot();
    apply_theme_snapshot(&provider, &initial);

    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    let current = Rc::new(RefCell::new(initial));
    glib::timeout_add_local(Duration::from_millis(500), move || {
        let next = active_theme_snapshot();
        if next != *current.borrow() {
            apply_theme_snapshot(&provider, &next);
            *current.borrow_mut() = next;
        }
        glib::ControlFlow::Continue
    });
}

fn active_theme_snapshot() -> (String, GtkAppThemeOptions) {
    let config = load_config();
    let settings = load_settings();
    (
        config.appearance.theme,
        GtkAppThemeOptions {
            font_scale: config.appearance.font_scale,
            animations: settings.appearance.animations,
            high_contrast: settings.appearance.high_contrast,
        },
    )
}

fn apply_theme_snapshot(provider: &gtk4::CssProvider, snapshot: &(String, GtkAppThemeOptions)) {
    let theme = theme_by_name(&snapshot.0);
    let css = format!("{}\n{}", gtk_app_css(&theme, snapshot.1), AI_CONSOLE_CSS);
    provider.load_from_string(&css);
    if let Some(settings) = gtk4::Settings::default() {
        settings.set_gtk_enable_animations(snapshot.1.animations);
        settings.set_gtk_application_prefer_dark_theme(gtk_app_prefers_dark(&theme));
    }
}

fn state_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("ai_console.json")
}

fn load_state() -> PersistedState {
    let path = state_path();
    let mut state = match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_default(),
        Err(_) => PersistedState::default(),
    };

    compact_placeholder_conversations(&mut state);
    state
}

fn compact_placeholder_conversations(state: &mut PersistedState) {
    if state.conversations.len() <= 1 {
        state.app_state.active_conversation = state.app_state.active_conversation.min(0);
        return;
    }

    let mut kept = Vec::with_capacity(state.conversations.len());
    let mut active_index = None;
    let mut kept_first_placeholder = false;

    for (index, conversation) in state.conversations.iter().cloned().enumerate() {
        let placeholder = is_placeholder_conversation(&conversation);
        let keep = if placeholder {
            if kept_first_placeholder {
                false
            } else {
                kept_first_placeholder = true;
                true
            }
        } else {
            true
        };

        if keep {
            if index == state.app_state.active_conversation {
                active_index = Some(kept.len());
            }
            kept.push(conversation);
        }
    }

    if kept.is_empty() {
        kept.push(Conversation {
            title: "New Chat 1".to_string(),
            summary: "Empty thread".to_string(),
            messages: Vec::new(),
        });
        active_index = Some(0);
    } else if active_index.is_none() {
        active_index = Some(0);
    }

    state.conversations = kept;
    state.app_state.active_conversation = active_index.unwrap_or(0);
}

fn persist_state(state: &PersistedState) {
    let path = state_path();
    if let Some(parent) = path.parent() {
        if fs::create_dir_all(parent).is_err() {
            return;
        }
        if parent.file_name().is_some_and(|name| name == "focaldesk")
            && fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).is_err()
        {
            return;
        }
    }
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = write_private_atomic(&path, text.as_bytes());
    }
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "AI Console state path has no parent",
        )
    })?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temp = parent.join(format!(".ai-console-{}-{stamp}.tmp", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conversation(title: &str, messages: &[&str]) -> Conversation {
        Conversation {
            title: title.to_string(),
            summary: String::new(),
            messages: messages.iter().map(|message| message.to_string()).collect(),
        }
    }

    #[test]
    fn chat_request_preserves_history_order_and_ends_with_current_prompt() {
        let store = PersistedState {
            conversations: vec![conversation(
                "Current thread",
                &[
                    "User: first",
                    "AI: first reply",
                    "User: second",
                    "AI: second reply",
                ],
            )],
            app_state: AppState::default(),
        };

        let request = build_chat_request(&store, 0, "current");
        let turns = request
            .messages
            .iter()
            .map(|message| (message.role.as_str(), message.content.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            turns,
            vec![
                (
                    "system",
                    "You are the FocalDesk AI Console. Keep responses concise. Respond in English unless the user requests another language."
                ),
                ("system", "Conversation: Current thread"),
                ("user", "first"),
                ("assistant", "first reply"),
                ("user", "second"),
                ("assistant", "second reply"),
                ("user", "current"),
            ]
        );
    }

    #[test]
    fn chat_request_uses_only_the_resolved_conversation() {
        let mut app_state = AppState::default();
        app_state.active_conversation = 0;
        let store = PersistedState {
            conversations: vec![
                conversation("Wrong thread", &["User: contaminated"]),
                conversation("Resolved thread", &["User: isolated"]),
            ],
            app_state,
        };

        let request = build_chat_request(&store, 1, "current");
        let contents = request
            .messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>();

        assert!(contents.contains(&"Conversation: Resolved thread"));
        assert!(contents.contains(&"isolated"));
        assert!(!contents.contains(&"contaminated"));
    }
}
