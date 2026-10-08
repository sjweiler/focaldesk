use anyhow::Context;
use focaldesk_ai::{
    AgentBuilder, AgentDefinition, AgentRequest, AgentResponse, AgentRunEventKind, AgentRunState,
    AgentRunStatus, AgentTrigger, AgentTriggerKind, AiDaemonStatus, AiIpcRequest, AiIpcResponse,
    AiStreamEvent, CapabilityPolicy, ChatMessage, ChatRequest, DirectoryIngestResult,
    DocumentIngestResult, IndexedDocument, MemoryId, MemoryStatus, ProviderInfo, ProviderModelInfo,
    ProviderTelemetry, SearchHit, cancel_ai_stream, send_ai_request, stream_ai_chat,
};
use focaldesk_config::load_config;
use focaldesk_gtk::{StateKind, StateView, StatusBanner};
use focaldesk_ipc::{
    ConnectorHostIpcRequest, ConnectorHostIpcResponse, IpcRequest, IpcResponse, MicrophoneEvent,
    MicrophoneIpcRequest, MicrophoneIpcResponse, NotificationIpcRequest, NotificationIpcResponse,
    SpeechIpcRequest, send_connector_host_request, send_desktop_request, send_microphone_request,
    send_notification_request, send_speech_request,
};
use focaldesk_settings_core::load_settings;
use focaldesk_themes::{GtkAppThemeOptions, gtk_app_css, gtk_app_prefers_dark, theme_by_name};
use focaldesk_voice::VoiceEvent;
use glib::ControlFlow;
use gtk4::prelude::*;
use gtk4::{
    Application, ApplicationWindow, Box, Button, CheckButton, DropDown, Entry, Label, Orientation,
    Paned, Revealer, ScrolledWindow, StringList, Switch, TextBuffer, TextView,
};
use serde::{Deserialize, Serialize};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Command,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Serialize, Deserialize)]
struct Conversation {
    title: String,
    summary: String,
    messages: Vec<String>,
}

#[derive(Clone)]
struct ChatStreamControls {
    stop_button: Button,
    active_request_id: Rc<RefCell<Option<String>>>,
}

struct AmbientMicClient {
    lease_id: String,
    stop_poll: Arc<AtomicBool>,
}

impl AmbientMicClient {
    fn stop(&self) {
        let lease_id = self.lease_id.clone();
        thread::spawn(move || {
            let _ = send_microphone_request(&MicrophoneIpcRequest::Stop {
                lease_id: Some(lease_id),
            });
        });
    }
}

impl Drop for AmbientMicClient {
    fn drop(&mut self) {
        self.stop_poll.store(true, Ordering::SeqCst);
    }
}

fn start_daemon_dictation(
    requester: &str,
) -> Result<(AmbientMicClient, mpsc::Receiver<VoiceEvent>), String> {
    let response = send_microphone_request(&MicrophoneIpcRequest::Start {
        requester: requester.to_string(),
    })?;
    if response.status == "error" {
        return Err(response
            .message
            .unwrap_or_else(|| "microphone request rejected".into()));
    }
    let lease_id = response
        .lease_id
        .ok_or_else(|| "voice daemon did not return a microphone lease".to_string())?;
    let stop_poll = Arc::new(AtomicBool::new(false));
    let (events_tx, events_rx) = mpsc::channel();
    let lease_for_poll = lease_id.clone();
    let stop_for_poll = stop_poll.clone();
    thread::spawn(move || {
        let mut after_sequence = 0;
        while !stop_for_poll.load(Ordering::SeqCst) {
            match send_microphone_request(&MicrophoneIpcRequest::Poll {
                lease_id: lease_for_poll.clone(),
                after_sequence,
            }) {
                Ok(response) => {
                    after_sequence = response.latest_sequence.max(after_sequence);
                    for record in response.events {
                        let event = match record.event {
                            MicrophoneEvent::Ready => VoiceEvent::Ready,
                            MicrophoneEvent::Partial(text) => VoiceEvent::Partial(text),
                            MicrophoneEvent::Final(text) => VoiceEvent::Final(text),
                            MicrophoneEvent::VoiceActivity(active) => {
                                VoiceEvent::VoiceActivity(active)
                            }
                            MicrophoneEvent::WakeDetected => VoiceEvent::WakeDetected,
                            MicrophoneEvent::Command(command) => VoiceEvent::Command(command),
                            MicrophoneEvent::Stopped => VoiceEvent::Stopped,
                            MicrophoneEvent::Error(error) => VoiceEvent::Error(error),
                        };
                        if events_tx.send(event).is_err() {
                            return;
                        }
                    }
                    if response.status == "idle" || response.killed {
                        return;
                    }
                }
                Err(error) => {
                    let _ = events_tx.send(VoiceEvent::Error(error));
                    return;
                }
            }
            thread::sleep(Duration::from_millis(80));
        }
    });
    Ok((
        AmbientMicClient {
            lease_id,
            stop_poll,
        },
        events_rx,
    ))
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
    #[serde(default)]
    use_memory: bool,
    #[serde(default = "default_coding_agent")]
    default_coding_agent: String,
    #[serde(default = "default_coding_agent_working_directory")]
    coding_agent_working_directory: String,
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
struct DesktopAgentPage {
    page: Box,
    agent_profile: ChoiceDropDown,
    objective: Entry,
    status: Label,
    result_buffer: TextBuffer,
    run_button: Button,
    cancel_button: Button,
    approve_button: Button,
    deny_button: Button,
    pending_plan_id: Rc<RefCell<Option<String>>>,
    active_run_id: Rc<RefCell<Option<String>>>,
}

#[derive(Clone)]
struct ConsoleNavigation {
    stack: gtk4::Stack,
    composer: Box,
    active_nav: Rc<RefCell<String>>,
    nav_buttons: Rc<RefCell<Vec<Button>>>,
}

impl ConsoleNavigation {
    fn show_desktop_agent(&self) {
        set_active_nav("Desktop Agent", &self.active_nav, &self.nav_buttons);
        self.composer.set_visible(false);
        self.stack.set_visible_child_name("desktop-agent");
    }
}

#[derive(Clone)]
struct ChoiceDropDown {
    widget: DropDown,
    ids: Rc<RefCell<Vec<String>>>,
}

impl ChoiceDropDown {
    fn new() -> Self {
        Self {
            widget: DropDown::from_strings(&[]),
            ids: Rc::new(RefCell::new(Vec::new())),
        }
    }

    fn replace(&self, choices: Vec<(String, String)>, selected_id: &str) {
        let selected = choices
            .iter()
            .position(|(id, _)| id == selected_id)
            .unwrap_or(0) as u32;
        let labels = choices
            .iter()
            .map(|(_, label)| label.as_str())
            .collect::<Vec<_>>();
        self.widget.set_model(Some(&StringList::new(&labels)));
        *self.ids.borrow_mut() = choices.into_iter().map(|(id, _)| id).collect();
        self.widget.set_selected(selected);
    }

    fn selected_id(&self) -> Option<String> {
        self.ids
            .borrow()
            .get(self.widget.selected() as usize)
            .cloned()
    }

    fn connect_changed<F: Fn(&Self) + 'static>(&self, callback: F) {
        let this = self.clone();
        self.widget
            .connect_selected_notify(move |_| callback(&this));
    }
}

#[derive(Clone)]
struct BackendBannerHandles {
    title_label: Label,
    subtitle_label: Label,
    backend_combo: ChoiceDropDown,
    model_combo: ChoiceDropDown,
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
            use_memory: false,
            default_coding_agent: default_coding_agent(),
            coding_agent_working_directory: default_coding_agent_working_directory(),
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
    if state.app_state.active_provider.is_empty()
        && let Some(default_provider) = runtime.default_provider.as_ref()
    {
        state.app_state.active_provider = default_provider.clone();
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
    let requested_agent = Rc::new(Cell::new(false));
    let app = Application::builder()
        .application_id("dev.focaldesk.AiConsole")
        .flags(gtk4::gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();

    let main_window = Rc::new(RefCell::new(None));
    let navigation = Rc::new(RefCell::new(None));
    {
        let requested_agent = requested_agent.clone();
        app.connect_command_line(move |app, command_line| {
            requested_agent.set(
                command_line
                    .arguments()
                    .iter()
                    .skip(1)
                    .any(|argument| argument == "--agent"),
            );
            app.activate();
            0
        });
    }
    let main_window_for_activate = main_window.clone();
    let navigation_for_activate = navigation.clone();
    app.connect_activate(move |app| {
        build_ui(
            app,
            main_window_for_activate.clone(),
            navigation_for_activate.clone(),
            requested_agent.replace(false),
        )
    });
    app.run();
}

fn build_ui(
    app: &Application,
    main_window: Rc<RefCell<Option<ApplicationWindow>>>,
    navigation: Rc<RefCell<Option<ConsoleNavigation>>>,
    open_agent: bool,
) {
    if let Some(window) = main_window.borrow().as_ref() {
        if open_agent && let Some(navigation) = navigation.borrow().as_ref() {
            navigation.show_desktop_agent();
        }
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
        "Desktop Agent",
        "Mission Control",
        "Scenario Lab",
        "Packages",
        "Agent Studio",
        "Context Fabric",
        "Event Fabric",
        "Attention",
        "Ambient Voice",
        "Coding Agents",
        "Conversations",
        "Providers",
        "Quick Prompts",
        "Memory",
        "Indexed Sources",
        "Settings",
        "Log/Debug",
    ];
    let initial_nav = if open_agent {
        "Desktop Agent"
    } else {
        "New Chat"
    };
    let active_nav = Rc::new(std::cell::RefCell::new(String::from(initial_nav)));
    let nav_buttons = Rc::new(std::cell::RefCell::new(Vec::<Button>::new()));

    for item in nav_items {
        let btn = Button::with_label(item);
        btn.add_css_class("sidebar-button");
        if item == initial_nav {
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
    let stop = Button::with_label("Stop");
    stop.set_sensitive(false);
    let stream_controls = ChatStreamControls {
        stop_button: stop.clone(),
        active_request_id: Rc::new(RefCell::new(None)),
    };
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
            stream_controls.clone(),
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
        &indexed_sources_page(log_buffer.clone()),
        Some("indexed-sources"),
        "Indexed Sources",
    );
    stack.add_titled(
        &settings_page(state.clone(), log_buffer.clone()),
        Some("settings"),
        "Settings",
    );
    stack.add_titled(&debug_page(log_buffer.clone()), Some("debug"), "Log/Debug");

    let desktop_agent_page =
        build_desktop_agent_page(state.clone(), runtime.clone(), log_buffer.clone());
    stack.add_titled(
        &desktop_agent_page.page,
        Some("desktop-agent"),
        "Desktop Agent",
    );
    stack.add_titled(
        &build_mission_control_page(log_buffer.clone()),
        Some("mission-control"),
        "Mission Control",
    );
    stack.add_titled(
        &build_scenario_lab_page(log_buffer.clone()),
        Some("scenario-lab"),
        "Scenario Lab",
    );
    stack.add_titled(
        &build_packages_page(log_buffer.clone()),
        Some("packages"),
        "Packages",
    );
    stack.add_titled(
        &build_agent_studio_page(log_buffer.clone()),
        Some("agent-studio"),
        "Agent Studio",
    );
    stack.add_titled(
        &build_context_fabric_page(state.clone(), log_buffer.clone()),
        Some("context-fabric"),
        "Context Fabric",
    );
    stack.add_titled(
        &build_event_fabric_page(log_buffer.clone()),
        Some("event-fabric"),
        "Event Fabric",
    );
    stack.add_titled(
        &build_attention_page(log_buffer.clone()),
        Some("attention"),
        "Attention",
    );
    stack.add_titled(
        &build_ambient_voice_page(
            state.clone(),
            chat_view.clone(),
            conversation_detail.clone(),
            entry.clone(),
            send.clone(),
            stream_controls.clone(),
            log_buffer.clone(),
        ),
        Some("ambient-voice"),
        "Ambient Voice",
    );
    stack.add_titled(
        &build_coding_agents_page(state.clone(), log_buffer.clone()),
        Some("coding-agents"),
        "Coding Agents",
    );

    let entry_clone = entry.clone();
    let state_clone = state.clone();
    let chat_view_clone = chat_view.clone();
    let conversation_detail_clone = conversation_detail.clone();
    let log_buffer_clone = log_buffer.clone();
    let send_button = send.clone();
    let stream_controls_for_send = stream_controls.clone();
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
            stream_controls_for_send.clone(),
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
        let stream_controls = stream_controls.clone();
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
                stream_controls.clone(),
                log_buffer.clone(),
                text,
                "manual chat",
                None,
            );
        });
    }

    {
        let active_request_id = stream_controls.active_request_id.clone();
        let stop_button = stop.clone();
        let log_buffer = log_buffer.clone();
        stop.connect_clicked(move |_| {
            let Some(request_id) = active_request_id.borrow().clone() else {
                return;
            };
            stop_button.set_sensitive(false);
            append_log(&log_buffer, "[chat] cancellation requested");
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(cancel_ai_stream(&request_id));
            });
            let active_request_id = active_request_id.clone();
            let stop_button = stop_button.clone();
            let log_buffer = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(true)) => ControlFlow::Break,
                Ok(Ok(false)) => {
                    append_log(&log_buffer, "[chat] stream already finished");
                    ControlFlow::Break
                }
                Ok(Err(err)) => {
                    append_log(&log_buffer, &format!("[chat] cancellation failed: {err}"));
                    if active_request_id.borrow().is_some() {
                        stop_button.set_sensitive(true);
                    }
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if active_request_id.borrow().is_some() {
                        stop_button.set_sensitive(true);
                    }
                    ControlFlow::Break
                }
            });
        });
    }

    {
        let current_session: Rc<RefCell<Option<AmbientMicClient>>> = Rc::new(RefCell::new(None));
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

            let (session, rx) = match start_daemon_dictation("focaldesk-ai-console/chat") {
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
                        Ok(VoiceEvent::Stopped) => {
                            button_for_poll.set_label("Voice");
                            *current_session_for_poll.borrow_mut() = None;
                            return ControlFlow::Break;
                        }
                        Ok(
                            VoiceEvent::VoiceActivity(_)
                            | VoiceEvent::WakeDetected
                            | VoiceEvent::Command(_),
                        ) => {}
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
    composer.append(&stop);
    composer.append(&send);

    stack.add_titled(&new_chat_workspace, Some("new-chat"), "New Chat");
    stack.set_visible_child_name(if open_agent {
        "desktop-agent"
    } else {
        "new-chat"
    });
    composer.set_visible(!open_agent);

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
        let composer = composer.clone();

        button.connect_clicked(move |_| {
            set_active_nav(&label, &active_nav, &nav_buttons);
            let page_name = match label.as_str() {
                "New Chat" => "new-chat",
                "Desktop Agent" => "desktop-agent",
                "Mission Control" => "mission-control",
                "Scenario Lab" => "scenario-lab",
                "Packages" => "packages",
                "Agent Studio" => "agent-studio",
                "Context Fabric" => "context-fabric",
                "Event Fabric" => "event-fabric",
                "Attention" => "attention",
                "Ambient Voice" => "ambient-voice",
                "Coding Agents" => "coding-agents",
                "Conversations" => "conversations",
                "Providers" => "providers",
                "Quick Prompts" => "prompts",
                "Memory" => "memory",
                "Indexed Sources" => "indexed-sources",
                "Settings" => "settings",
                "Log/Debug" => "debug",
                _ => "new-chat",
            };

            if label.as_str() == "New Chat" {
                // Navigation should not manufacture a conversation.
                render_active_conversation(&chat_view, &state.borrow());
            }

            composer.set_visible(!matches!(
                label.as_str(),
                "Desktop Agent"
                    | "Mission Control"
                    | "Scenario Lab"
                    | "Packages"
                    | "Agent Studio"
                    | "Context Fabric"
                    | "Event Fabric"
                    | "Attention"
                    | "Ambient Voice"
                    | "Coding Agents"
            ));
            stack.set_visible_child_name(page_name);
        });
    }

    root.append(&sidebar);
    root.append(&main);

    *navigation.borrow_mut() = Some(ConsoleNavigation {
        stack: stack.clone(),
        composer: composer.clone(),
        active_nav,
        nav_buttons,
    });
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

fn build_mission_control_page(log_buffer: TextBuffer) -> Box {
    let page = section_shell(
        "AIOS Mission Control",
        "A minimized, unified view of events, context provenance, routines, agents, workflows, capability leases, and audited control changes.",
    );
    page.append(&info_card(&[
        "Timeline entries never include tool arguments, tool results, context payloads, or raw failure text.".into(),
        "Emergency pause blocks new triggers and routine dispatch, disconnects Event Fabric, and asks the connector host to pause. Active runs require explicit cancellation.".into(),
        "Replay is simulation-only and cannot retain another event or execute an action.".into(),
    ]));

    let status = Label::new(Some("Mission Control is loading…"));
    status.set_xalign(0.0);
    status.set_wrap(true);
    page.append(&status);

    let search_row = Box::new(Orientation::Horizontal, 8);
    let search = Entry::builder()
        .placeholder_text("Search redacted timeline and audit metadata")
        .hexpand(true)
        .build();
    let refresh = Button::with_label("Refresh");
    let live = CheckButton::with_label("Live refresh");
    live.set_active(true);
    let emergency_pause = Button::with_label("Emergency pause");
    emergency_pause.add_css_class("destructive-action");
    search_row.append(&search);
    search_row.append(&refresh);
    search_row.append(&live);
    search_row.append(&emergency_pause);
    page.append(&search_row);

    let control_row = Box::new(Orientation::Horizontal, 8);
    let agent_run_id = Entry::builder()
        .placeholder_text("Agent run ID")
        .hexpand(true)
        .build();
    let cancel_agent = Button::with_label("Cancel agent");
    let workflow_run_id = Entry::builder()
        .placeholder_text("Workflow run ID")
        .hexpand(true)
        .build();
    let cancel_workflow = Button::with_label("Cancel workflow");
    control_row.append(&agent_run_id);
    control_row.append(&cancel_agent);
    control_row.append(&workflow_run_id);
    control_row.append(&cancel_workflow);
    page.append(&control_row);

    let replay_row = Box::new(Orientation::Horizontal, 8);
    let event_id = Entry::builder()
        .placeholder_text("Retained event ID")
        .hexpand(true)
        .build();
    let replay = Button::with_label("Replay as simulation");
    replay_row.append(&event_id);
    replay_row.append(&replay);
    page.append(&replay_row);

    let output = TextBuffer::new(None);
    output.set_text("Waiting for the first bounded snapshot.");
    let view = TextView::with_buffer(&output);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&view)
            .height_request(560)
            .hexpand(true)
            .vexpand(true)
            .build(),
    );

    let in_flight = Rc::new(Cell::new(false));
    {
        let status = status.clone();
        let output = output.clone();
        let search = search.clone();
        let log_buffer = log_buffer.clone();
        let in_flight = in_flight.clone();
        refresh.connect_clicked(move |_| {
            refresh_mission_control(
                search.text().trim().to_string(),
                status.clone(),
                output.clone(),
                log_buffer.clone(),
                in_flight.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let search = search.clone();
        let log_buffer = log_buffer.clone();
        let in_flight = in_flight.clone();
        search.connect_activate(move |entry| {
            refresh_mission_control(
                entry.text().trim().to_string(),
                status.clone(),
                output.clone(),
                log_buffer.clone(),
                in_flight.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let search = search.clone();
        let log_buffer = log_buffer.clone();
        let in_flight = in_flight.clone();
        let live = live.clone();
        glib::timeout_add_local(Duration::from_secs(2), move || {
            if live.is_active() {
                refresh_mission_control(
                    search.text().trim().to_string(),
                    status.clone(),
                    output.clone(),
                    log_buffer.clone(),
                    in_flight.clone(),
                );
            }
            ControlFlow::Continue
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let in_flight = in_flight.clone();
        emergency_pause.connect_clicked(move |_| {
            dispatch_mission_control_request(
                AiIpcRequest::ActivateMissionControlPause,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
                in_flight.clone(),
                true,
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let in_flight = in_flight.clone();
        cancel_agent.connect_clicked(move |_| {
            dispatch_mission_control_request(
                AiIpcRequest::CancelAgentRun {
                    run_id: agent_run_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
                in_flight.clone(),
                false,
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let in_flight = in_flight.clone();
        cancel_workflow.connect_clicked(move |_| {
            dispatch_mission_control_request(
                AiIpcRequest::CancelWorkflow {
                    run_id: workflow_run_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
                in_flight.clone(),
                false,
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let in_flight = in_flight.clone();
        replay.connect_clicked(move |_| {
            dispatch_mission_control_request(
                AiIpcRequest::ReplayEventSimulation {
                    event_id: event_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
                in_flight.clone(),
                false,
            );
        });
    }

    refresh_mission_control(String::new(), status, output, log_buffer, in_flight);
    page
}

fn refresh_mission_control(
    query: String,
    status: Label,
    output: TextBuffer,
    log_buffer: TextBuffer,
    in_flight: Rc<Cell<bool>>,
) {
    dispatch_mission_control_request(
        AiIpcRequest::GetMissionControl {
            query: (!query.is_empty()).then_some(query),
            limit: 100,
        },
        status,
        output,
        log_buffer,
        in_flight,
        false,
    );
}

fn dispatch_mission_control_request(
    request: AiIpcRequest,
    status: Label,
    output: TextBuffer,
    log_buffer: TextBuffer,
    in_flight: Rc<Cell<bool>>,
    pause_connector_host: bool,
) {
    if in_flight.replace(true) {
        return;
    }
    let include_runtime_state = matches!(
        &request,
        AiIpcRequest::GetMissionControl { .. } | AiIpcRequest::ActivateMissionControlPause
    );
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let response = send_ai_request(&request).map_err(|error| error.to_string());
        let connector_host = if pause_connector_host && response.is_ok() {
            Some(send_connector_host_request(
                &ConnectorHostIpcRequest::SetPaused { paused: true },
            ))
        } else if include_runtime_state {
            Some(send_connector_host_request(
                &ConnectorHostIpcRequest::Status,
            ))
        } else {
            None
        };
        let microphone =
            include_runtime_state.then(|| send_microphone_request(&MicrophoneIpcRequest::Status));
        let _ = tx.send((response, connector_host, microphone));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok((Ok(AiIpcResponse::MissionControlState { state }), connector_host, microphone)) => {
            output.set_text(&render_mission_control(
                &state,
                connector_host
                    .as_ref()
                    .and_then(|result| result.as_ref().ok()),
                microphone.as_ref().and_then(|result| result.as_ref().ok()),
            ));
            let connector_note = if pause_connector_host {
                match connector_host {
                    Some(Ok(response)) if response.status == "ok" => " Connector host paused.",
                    Some(Ok(_)) => {
                        " Connector host pause failed; Event Fabric remains disconnected."
                    }
                    Some(Err(_)) => {
                        " Connector host unavailable; Event Fabric remains disconnected."
                    }
                    None => " Connector host pause was not attempted.",
                }
            } else {
                ""
            };
            status.set_text(&format!(
                "Snapshot: {} active runs, {} timeline entries.{}",
                state.active_runs.len(),
                state.timeline.len(),
                connector_note
            ));
            in_flight.set(false);
            ControlFlow::Break
        }
        Ok((Ok(AiIpcResponse::AgentRunCancellation { accepted, .. }), _, _)) => {
            status.set_text(if accepted {
                "Agent cancellation accepted."
            } else {
                "Agent run is not active or cannot be cancelled."
            });
            in_flight.set(false);
            ControlFlow::Break
        }
        Ok((Ok(AiIpcResponse::WorkflowControl { accepted, .. }), _, _)) => {
            status.set_text(if accepted {
                "Workflow cancellation accepted."
            } else {
                "Workflow run is not active or cannot be cancelled."
            });
            in_flight.set(false);
            ControlFlow::Break
        }
        Ok((Ok(AiIpcResponse::EventDelivered { delivery }), _, _)) => {
            output.set_text(&format!(
                "SIMULATION ONLY\nevent={}\nsource={}\nsummary={}\nevaluations={:#?}",
                delivery.event.id,
                delivery.event.source.as_str(),
                delivery.event.summary,
                delivery.evaluations
            ));
            status.set_text("Retained redacted event replayed as a non-mutating simulation.");
            in_flight.set(false);
            ControlFlow::Break
        }
        Ok((Ok(AiIpcResponse::Error { message }), _, _)) | Ok((Err(message), _, _)) => {
            status.set_text(&format!("Mission Control request failed: {message}"));
            append_log(&log_buffer, &format!("[mission-control] {message}"));
            in_flight.set(false);
            ControlFlow::Break
        }
        Ok((Ok(other), _, _)) => {
            status.set_text(&format!("Unexpected Mission Control response: {other:?}"));
            in_flight.set(false);
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Mission Control request channel disconnected.");
            in_flight.set(false);
            ControlFlow::Break
        }
    });
}

fn render_mission_control(
    state: &focaldesk_ai::MissionControlSnapshot,
    connector_host: Option<&ConnectorHostIpcResponse>,
    microphone: Option<&MicrophoneIpcResponse>,
) -> String {
    let mut lines = vec![
        format!(
            "SAFETY  global={}  triggers={}  routines={}  event_fabric={}",
            if state.globally_paused {
                "PAUSED"
            } else {
                "active"
            },
            if state.triggers_suspended {
                "paused"
            } else {
                "active"
            },
            if state.routines_suspended {
                "paused"
            } else {
                "active"
            },
            if state.event_fabric_connected {
                "connected"
            } else {
                "disconnected"
            },
        ),
        format!(
            "INVENTORY  runs={} leases={} context_grants={} connectors={} suggestions={}",
            state.active_runs.len(),
            state.active_leases.len(),
            state.active_context_grants.len(),
            state.enabled_connectors.len(),
            state.pending_suggestions,
        ),
        String::new(),
        "ACTIVE RUNTIMES".into(),
    ];
    lines.push(format!(
        "MICROPHONE  status={} owner={} killed={}",
        microphone.map_or("unavailable", |state| state.status.as_str()),
        microphone
            .and_then(|state| state.owner.as_deref())
            .unwrap_or("none"),
        microphone.is_some_and(|state| state.killed),
    ));
    if state.active_runs.is_empty() {
        lines.push("  none".into());
    }
    for run in &state.active_runs {
        lines.push(format!(
            "  {} {} owner={} state={} deadline={}",
            run.kind, run.run_id, run.owner_id, run.state, run.deadline_at_unix
        ));
    }
    lines.push(String::new());
    lines.push("CAPABILITY LEASES".into());
    if state.active_leases.is_empty() {
        lines.push("  none".into());
    }
    for lease in &state.active_leases {
        lines.push(format!(
            "  {} agent={} run={} tools={} expires={}",
            lease.lease_id,
            lease.agent_id,
            lease.run_id,
            lease.tools.join(","),
            lease.expires_at_unix
        ));
    }
    lines.push(String::new());
    lines.push("CONTEXT GRANTS".into());
    if state.active_context_grants.is_empty() {
        lines.push("  none".into());
    }
    for grant in &state.active_context_grants {
        lines.push(format!(
            "  {} agent={} kinds={:?} expires={}",
            grant.id, grant.agent_id, grant.kinds, grant.expires_at_unix
        ));
    }
    lines.push(String::new());
    lines.push("AGENT BUDGETS".into());
    for budget in &state.budgets {
        lines.push(format!(
            "  {} enabled={} runs={} failures={} tokens={}/{} cost_microusd={}/{}",
            budget.agent_id,
            budget.enabled,
            budget.runs_today,
            budget.failures_today,
            budget.tokens_used_today,
            budget
                .token_limit
                .map_or_else(|| "unbounded".into(), |value| value.to_string()),
            budget.cost_used_microusd_today,
            budget
                .cost_limit_microusd
                .map_or_else(|| "unbounded".into(), |value| value.to_string()),
        ));
    }
    lines.push(String::new());
    lines.push("CONNECTORS".into());
    for connector in &state.connectors {
        lines.push(format!(
            "  {} enabled={} health={} network={} last_event={}",
            connector.connector_id,
            connector.enabled,
            connector.health,
            connector.network_allowed,
            connector
                .last_event_at_unix
                .map_or_else(|| "never".into(), |value| value.to_string()),
        ));
    }
    if let Some(host) = connector_host {
        lines.push(format!("  host paused={}", host.paused));
        for runtime in &host.connectors {
            lines.push(format!(
                "    {} state={} failures={} quarantined={} delivered={}",
                runtime.connector_id,
                runtime.state,
                runtime.consecutive_failures,
                runtime.quarantined,
                runtime.delivered_events,
            ));
        }
    } else {
        lines.push("  host unavailable".into());
    }
    lines.push(String::new());
    lines.push("TIMELINE (newest first)".into());
    if state.timeline.is_empty() {
        lines.push("  no matching entries".into());
    }
    for entry in &state.timeline {
        lines.push(format!(
            "[{}] {:?} {}  {}",
            entry.at_unix, entry.kind, entry.state, entry.title
        ));
        lines.push(format!("  {}", entry.summary));
        lines.push(format!("  via {}  id={}", entry.provenance, entry.id));
    }
    lines.join("\n")
}

fn build_scenario_lab_page(log_buffer: TextBuffer) -> Box {
    let page = section_shell(
        "AIOS Scenario Lab",
        "Author and replay deterministic safety fixtures in a shadow evaluator with zero provider calls, tool executions, or live mutations.",
    );
    page.append(&info_card(&[
        "Fixtures must be explicitly marked synthetic and cannot contain context values, tool arguments, tool results, secrets, or executable actions.".into(),
        "Mission Control capture records only minimized timeline labels, state, and provenance.".into(),
        "A scenario passes when its observed violation-code set exactly matches its expected set.".into(),
    ]));
    let status = Label::new(Some(
        "Edit the fixture or capture a minimized Mission Control trace.",
    ));
    status.set_xalign(0.0);
    status.set_wrap(true);
    page.append(&status);

    let capture_row = Box::new(Orientation::Horizontal, 8);
    let scenario_name = Entry::builder()
        .placeholder_text("Scenario name")
        .text("captured-mission-trace")
        .hexpand(true)
        .build();
    let capture_query = Entry::builder()
        .placeholder_text("Optional Mission Control filter")
        .hexpand(true)
        .build();
    let capture = Button::with_label("Capture minimized trace");
    let load_example = Button::with_label("Load safe example");
    capture_row.append(&scenario_name);
    capture_row.append(&capture_query);
    capture_row.append(&capture);
    capture_row.append(&load_example);
    page.append(&capture_row);

    let fixture_buffer = TextBuffer::new(None);
    fixture_buffer.set_text(scenario_example_json());
    let fixture_view = TextView::with_buffer(&fixture_buffer);
    fixture_view.set_monospace(true);
    fixture_view.set_wrap_mode(gtk4::WrapMode::None);
    page.append(
        &ScrolledWindow::builder()
            .child(&fixture_view)
            .height_request(360)
            .hexpand(true)
            .build(),
    );
    let evaluate = Button::with_label("Evaluate isolated scenario");
    evaluate.add_css_class("suggested-action");
    page.append(&evaluate);

    let report_buffer = TextBuffer::new(None);
    report_buffer.set_text("No scenario has been evaluated.");
    let report_view = TextView::with_buffer(&report_buffer);
    report_view.set_editable(false);
    report_view.set_cursor_visible(false);
    report_view.set_monospace(true);
    report_view.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&report_view)
            .height_request(300)
            .hexpand(true)
            .build(),
    );

    {
        let fixture_buffer = fixture_buffer.clone();
        load_example.connect_clicked(move |_| fixture_buffer.set_text(scenario_example_json()));
    }
    {
        let status = status.clone();
        let fixture_buffer = fixture_buffer.clone();
        let report_buffer = report_buffer.clone();
        let log_buffer = log_buffer.clone();
        evaluate.connect_clicked(move |_| {
            let text = fixture_buffer.text(
                &fixture_buffer.start_iter(),
                &fixture_buffer.end_iter(),
                false,
            );
            let fixture = match serde_json::from_str::<focaldesk_ai::ScenarioFixture>(&text) {
                Ok(fixture) => fixture,
                Err(error) => {
                    status.set_text(&format!("Fixture JSON is invalid: {error}"));
                    return;
                }
            };
            dispatch_scenario_request(
                AiIpcRequest::EvaluateScenario {
                    fixture: std::boxed::Box::new(fixture),
                },
                status.clone(),
                fixture_buffer.clone(),
                report_buffer.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let fixture_buffer = fixture_buffer.clone();
        let report_buffer = report_buffer.clone();
        let log_buffer = log_buffer.clone();
        capture.connect_clicked(move |_| {
            let query = capture_query.text().trim().to_string();
            dispatch_scenario_request(
                AiIpcRequest::CaptureScenario {
                    name: scenario_name.text().trim().to_string(),
                    query: (!query.is_empty()).then_some(query),
                    limit: 100,
                },
                status.clone(),
                fixture_buffer.clone(),
                report_buffer.clone(),
                log_buffer.clone(),
            );
        });
    }
    page
}

fn dispatch_scenario_request(
    request: AiIpcRequest,
    status: Label,
    fixture_buffer: TextBuffer,
    report_buffer: TextBuffer,
    log_buffer: TextBuffer,
) {
    status.set_text("Scenario Lab request in progress…");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_ai_request(&request).map_err(|error| error.to_string()));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::ScenarioEvaluated { report })) => {
            report_buffer.set_text(
                &serde_json::to_string_pretty(&report).unwrap_or_else(|_| format!("{report:#?}")),
            );
            status.set_text(if report.passed {
                "PASS — observed violations exactly match the fixture contract; no live execution occurred."
            } else {
                "FAIL — the observed safety violations differ from the expected contract."
            });
            append_log(
                &log_buffer,
                &format!("[scenario-lab] {} passed={}", report.name, report.passed),
            );
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::ScenarioCaptured { fixture })) => {
            fixture_buffer.set_text(
                &serde_json::to_string_pretty(&fixture)
                    .unwrap_or_else(|_| "scenario encoding failed".into()),
            );
            report_buffer.set_text("Captured fixture is inert until Evaluate is selected.");
            status.set_text("Captured a minimized Mission Control trace into an editable fixture.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            status.set_text(&format!("Scenario Lab request failed: {message}"));
            append_log(&log_buffer, &format!("[scenario-lab] {message}"));
            ControlFlow::Break
        }
        Ok(Ok(other)) => {
            status.set_text(&format!("Unexpected Scenario Lab response: {other:?}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Scenario Lab request channel disconnected.");
            ControlFlow::Break
        }
    });
}

fn build_packages_page(log_buffer: TextBuffer) -> Box {
    let page = section_shell(
        "AIOS Packages",
        "Inspect, trust, stage, activate, and roll back signed declarative .fai bundles.",
    );
    page.append(&info_card(&[
        "A valid Ed25519 signature, explicitly trusted signer, and passing Scenario Lab suite are required before staging.".into(),
        "Activation never grants network access, enables connectors, starts triggers, or executes packaged code.".into(),
        "Package agents start disabled after install, update, or rollback. Choose and enable them from Agent Studio after reviewing their declared authority.".into(),
        "Authority additions are shown before activation; an activation reload failure automatically restores the prior package.".into(),
    ]));

    let status = Label::new(Some("Paste a .fai JSON bundle to inspect it."));
    status.set_xalign(0.0);
    status.set_wrap(true);
    page.append(&status);

    let identity_row = Box::new(Orientation::Horizontal, 8);
    let package_id = Entry::builder()
        .placeholder_text("Package id")
        .hexpand(true)
        .build();
    let signer_id = Entry::builder()
        .placeholder_text("Signer id")
        .hexpand(true)
        .build();
    let public_key = Entry::builder()
        .placeholder_text("Ed25519 public key (hex)")
        .hexpand(true)
        .build();
    identity_row.append(&package_id);
    identity_row.append(&signer_id);
    identity_row.append(&public_key);
    page.append(&identity_row);

    let project_buffer = TextBuffer::new(None);
    project_buffer.set_text(
        &serde_json::to_string_pretty(&focaldesk_ai::FaiForgeProject::example(
            "focus-kit",
            "Focus Kit",
            "local-dev",
        ))
        .unwrap_or_else(|_| "Forge example encoding failed".into()),
    );
    let project_view = TextView::with_buffer(&project_buffer);
    project_view.set_monospace(true);
    project_view.set_wrap_mode(gtk4::WrapMode::None);
    page.append(
        &ScrolledWindow::builder()
            .child(&project_view)
            .height_request(300)
            .hexpand(true)
            .build(),
    );
    let forge_row = Box::new(Orientation::Horizontal, 8);
    let generate_signer = Button::with_label("Generate protected signer");
    let test_project = Button::with_label("Test project");
    let build_project = Button::with_label("Build signed bundle");
    build_project.add_css_class("suggested-action");
    forge_row.append(&generate_signer);
    forge_row.append(&test_project);
    forge_row.append(&build_project);
    page.append(&forge_row);

    let catalog_key = Entry::builder()
        .placeholder_text("Pinned private-registry catalog key (hex)")
        .hexpand(true)
        .build();
    page.append(&catalog_key);
    let catalog_buffer = TextBuffer::new(None);
    catalog_buffer.set_text("Paste an explicitly synced signed catalog here.");
    let catalog_view = TextView::with_buffer(&catalog_buffer);
    catalog_view.set_monospace(true);
    catalog_view.set_wrap_mode(gtk4::WrapMode::None);
    page.append(
        &ScrolledWindow::builder()
            .child(&catalog_view)
            .height_request(220)
            .hexpand(true)
            .build(),
    );
    let verify_catalog = Button::with_label("Verify and browse catalog");
    page.append(&verify_catalog);

    let bundle_buffer = TextBuffer::new(None);
    bundle_buffer.set_text("Paste a signed .fai JSON bundle here.");
    let bundle_view = TextView::with_buffer(&bundle_buffer);
    bundle_view.set_monospace(true);
    bundle_view.set_wrap_mode(gtk4::WrapMode::None);
    page.append(
        &ScrolledWindow::builder()
            .child(&bundle_view)
            .height_request(300)
            .hexpand(true)
            .build(),
    );

    let action_row = Box::new(Orientation::Horizontal, 8);
    let inspect = Button::with_label("Inspect bundle");
    let trust = Button::with_label("Trust signer");
    let stage = Button::with_label("Stage");
    let activate = Button::with_label("Activate");
    activate.add_css_class("suggested-action");
    let rollback = Button::with_label("Rollback");
    let list = Button::with_label("Refresh installed");
    for button in [&inspect, &trust, &stage, &activate, &rollback, &list] {
        action_row.append(button);
    }
    page.append(&action_row);

    let output_buffer = TextBuffer::new(None);
    output_buffer.set_text("No package inspection has run.");
    let output_view = TextView::with_buffer(&output_buffer);
    output_view.set_editable(false);
    output_view.set_cursor_visible(false);
    output_view.set_monospace(true);
    output_view.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&output_view)
            .height_request(260)
            .hexpand(true)
            .build(),
    );

    for (button, stage_bundle) in [(&inspect, false), (&stage, true)] {
        let status = status.clone();
        let bundle_buffer = bundle_buffer.clone();
        let output_buffer = output_buffer.clone();
        let log_buffer = log_buffer.clone();
        button.connect_clicked(move |_| {
            let text = bundle_buffer.text(
                &bundle_buffer.start_iter(),
                &bundle_buffer.end_iter(),
                false,
            );
            let bundle = match serde_json::from_str::<focaldesk_ai::FaiBundle>(&text) {
                Ok(bundle) => bundle,
                Err(error) => {
                    status.set_text(&format!("Bundle JSON is invalid: {error}"));
                    return;
                }
            };
            let request = if stage_bundle {
                AiIpcRequest::StagePackage {
                    bundle: std::boxed::Box::new(bundle),
                }
            } else {
                AiIpcRequest::InspectPackage {
                    bundle: std::boxed::Box::new(bundle),
                }
            };
            dispatch_package_request(
                request,
                status.clone(),
                output_buffer.clone(),
                Some(bundle_buffer.clone()),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output_buffer = output_buffer.clone();
        let bundle_buffer = bundle_buffer.clone();
        let signer_id = signer_id.clone();
        let public_key = public_key.clone();
        let log_buffer = log_buffer.clone();
        trust.connect_clicked(move |_| {
            dispatch_package_request(
                AiIpcRequest::TrustPackageSigner {
                    signer: focaldesk_ai::FaiSigner {
                        id: signer_id.text().trim().to_string(),
                        public_key_hex: public_key.text().trim().to_string(),
                    },
                },
                status.clone(),
                output_buffer.clone(),
                Some(bundle_buffer.clone()),
                log_buffer.clone(),
            );
        });
    }
    for (button, rollback_requested) in [(&activate, false), (&rollback, true)] {
        let status = status.clone();
        let package_id = package_id.clone();
        let output_buffer = output_buffer.clone();
        let bundle_buffer = bundle_buffer.clone();
        let log_buffer = log_buffer.clone();
        button.connect_clicked(move |_| {
            let package_id = package_id.text().trim().to_string();
            let request = if rollback_requested {
                AiIpcRequest::RollbackPackage { package_id }
            } else {
                AiIpcRequest::ActivatePackage { package_id }
            };
            dispatch_package_request(
                request,
                status.clone(),
                output_buffer.clone(),
                Some(bundle_buffer.clone()),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output_buffer = output_buffer.clone();
        let bundle_buffer = bundle_buffer.clone();
        let log_buffer = log_buffer.clone();
        list.connect_clicked(move |_| {
            dispatch_package_request(
                AiIpcRequest::ListPackages,
                status.clone(),
                output_buffer.clone(),
                Some(bundle_buffer.clone()),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let signer_id = signer_id.clone();
        let output_buffer = output_buffer.clone();
        let bundle_buffer = bundle_buffer.clone();
        let log_buffer = log_buffer.clone();
        generate_signer.connect_clicked(move |_| {
            dispatch_package_request(
                AiIpcRequest::GeneratePackageSigner {
                    signer_id: signer_id.text().trim().to_string(),
                },
                status.clone(),
                output_buffer.clone(),
                Some(bundle_buffer.clone()),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let project_buffer = project_buffer.clone();
        let output_buffer = output_buffer.clone();
        test_project.connect_clicked(move |_| {
            let text = project_buffer.text(
                &project_buffer.start_iter(),
                &project_buffer.end_iter(),
                false,
            );
            match serde_json::from_str::<focaldesk_ai::FaiForgeProject>(&text) {
                Ok(project) => match project.test() {
                    Ok(report) => {
                        output_buffer.set_text(
                            &serde_json::to_string_pretty(&report)
                                .unwrap_or_else(|_| format!("{report:#?}")),
                        );
                        status.set_text(if report.passed {
                            "Forge project passes its isolated Scenario Lab suite."
                        } else {
                            "Forge project failed its Scenario Lab suite."
                        });
                    }
                    Err(error) => status.set_text(&format!("Forge project is invalid: {error}")),
                },
                Err(error) => status.set_text(&format!("Forge project is invalid: {error}")),
            }
        });
    }
    {
        let status = status.clone();
        let project_buffer = project_buffer.clone();
        let output_buffer = output_buffer.clone();
        let bundle_buffer = bundle_buffer.clone();
        let log_buffer = log_buffer.clone();
        build_project.connect_clicked(move |_| {
            let text = project_buffer.text(
                &project_buffer.start_iter(),
                &project_buffer.end_iter(),
                false,
            );
            let project = match serde_json::from_str::<focaldesk_ai::FaiForgeProject>(&text) {
                Ok(project) => project,
                Err(error) => {
                    status.set_text(&format!("Forge project JSON is invalid: {error}"));
                    return;
                }
            };
            dispatch_package_request(
                AiIpcRequest::BuildPackageProject {
                    project: std::boxed::Box::new(project),
                },
                status.clone(),
                output_buffer.clone(),
                Some(bundle_buffer.clone()),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output_buffer = output_buffer.clone();
        verify_catalog.connect_clicked(move |_| {
            let text = catalog_buffer.text(
                &catalog_buffer.start_iter(),
                &catalog_buffer.end_iter(),
                false,
            );
            let catalog = match serde_json::from_str::<focaldesk_ai::FaiSignedCatalog>(&text) {
                Ok(catalog) => catalog,
                Err(error) => {
                    status.set_text(&format!("Signed catalog JSON is invalid: {error}"));
                    return;
                }
            };
            match focaldesk_ai::verify_registry_catalog(
                &catalog,
                catalog_key.text().trim(),
                None,
            ) {
                Ok(()) => {
                    let revoked = catalog
                        .catalog
                        .packages
                        .iter()
                        .filter(|entry| entry.revoked)
                        .count();
                    output_buffer.set_text(
                        &serde_json::to_string_pretty(&catalog.catalog.packages)
                            .unwrap_or_else(|_| "catalog encoding failed".into()),
                    );
                    status.set_text(&format!(
                        "Verified registry {} sequence {}; {} package versions, {} revoked. Downloads remain quarantined until separately inspected and staged.",
                        catalog.catalog.registry_id,
                        catalog.catalog.sequence,
                        catalog.catalog.packages.len(),
                        revoked
                    ));
                }
                Err(error) => status.set_text(&format!("Catalog verification failed: {error}")),
            }
        });
    }
    page
}

fn dispatch_package_request(
    request: AiIpcRequest,
    status: Label,
    output_buffer: TextBuffer,
    bundle_buffer: Option<TextBuffer>,
    log_buffer: TextBuffer,
) {
    status.set_text("Package request in progress…");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_ai_request(&request).map_err(|error| error.to_string()));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::PackageInspected { inspection })) => {
            output_buffer.set_text(
                &serde_json::to_string_pretty(&inspection)
                    .unwrap_or_else(|_| format!("{inspection:#?}")),
            );
            status.set_text(if inspection.signer_trusted && inspection.scenarios_passed {
                "Bundle verified. Review its authority summary before activation."
            } else {
                "Inspection complete. Trust and Scenario Lab requirements are not yet satisfied."
            });
            append_log(
                &log_buffer,
                &format!(
                    "[packages] inspected {} signature={} trusted={} scenarios={}",
                    inspection.package_id,
                    inspection.signature_valid,
                    inspection.signer_trusted,
                    inspection.scenarios_passed
                ),
            );
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::PackageSignerTrusted)) => {
            status.set_text("Signer trusted. Re-inspect the bundle before staging.");
            output_buffer.set_text("Signer trust saved to the private local package store.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::PackageSignerGenerated { signer })) => {
            output_buffer.set_text(
                &serde_json::to_string_pretty(&signer).unwrap_or_else(|_| format!("{signer:#?}")),
            );
            status.set_text(
                "Signer generated inside focald-secrets. Only the public key is displayed.",
            );
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::PackageBuilt { bundle })) => {
            let encoded = serde_json::to_string_pretty(&bundle)
                .unwrap_or_else(|_| "bundle encoding failed".into());
            if let Some(bundle_buffer) = &bundle_buffer {
                bundle_buffer.set_text(&encoded);
            }
            output_buffer.set_text("Signed bundle built and loaded into the bundle editor.");
            status.set_text(
                "Build complete. Inspect the authority summary before trusting or staging.",
            );
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::PackageActivated { bundle })) => {
            status.set_text("Package registry changed; packaged connectors remain disabled.");
            output_buffer.set_text(&format!(
                "{} {} is now active.",
                bundle.manifest.name, bundle.manifest.version
            ));
            append_log(
                &log_buffer,
                &format!(
                    "[packages] active {} {}",
                    bundle.manifest.id, bundle.manifest.version
                ),
            );
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Packages { packages })) => {
            output_buffer.set_text(
                &packages
                    .iter()
                    .map(|package| {
                        format!(
                            "{} — {}\nactive: {}  staged: {}  rollback available: {}\nsigner: {}",
                            package.name,
                            package.package_id,
                            package.active_version.as_deref().unwrap_or("none"),
                            package.staged_version.as_deref().unwrap_or("none"),
                            package.rollback_available,
                            package.signer_id,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n────────────────────────\n\n"),
            );
            status.set_text(&format!("Loaded {} installed packages.", packages.len()));
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            status.set_text(&format!("Package request failed: {message}"));
            append_log(&log_buffer, &format!("[packages] {message}"));
            ControlFlow::Break
        }
        Ok(Ok(other)) => {
            status.set_text(&format!("Unexpected package response: {other:?}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Package request channel disconnected.");
            ControlFlow::Break
        }
    });
}

fn scenario_example_json() -> &'static str {
    r#"{
  "scenario_version": 1,
  "name": "safe-meeting-preparation",
  "description": "Synthetic voice, connector, context, routine, and agent safety path.",
  "synthetic": true,
  "initial": {
    "event_fabric_connected": true,
    "routines_suspended": false,
    "source_fields": {
      "calendar": ["event", "title", "start_time"]
    },
    "connectors": {
      "calendar-fixture": {
        "enabled": true,
        "sources": {
          "calendar": ["event", "title", "start_time"]
        }
      }
    },
    "agent_tools": {
      "desktop": ["desktop_snapshot"]
    },
    "token_budget": 4000
  },
  "steps": [
    {
      "type": "voice_phrase",
      "text": "prepare for my next meeting",
      "expected_destination": "workflow:meeting-preparation"
    },
    {
      "type": "connector_event",
      "connector_id": "calendar-fixture",
      "source": "calendar",
      "payload": {
        "event": "calendar meeting",
        "title": "Synthetic planning review",
        "start_time": "2030-01-01T13:00:00Z"
      }
    },
    {
      "type": "context_metadata",
      "kind": "active_window",
      "provenance": "synthetic desktop fixture",
      "sensitivity": "private",
      "fields": ["app_id", "title"]
    },
    {
      "type": "routine_event",
      "event": {
        "kind": "calendar",
        "value": "calendar meeting starts soon",
        "source": "synthetic calendar fixture"
      }
    },
    {
      "type": "agent_plan",
      "agent_id": "desktop",
      "tools": ["desktop_snapshot"],
      "proposed_mutation": false,
      "confirmation_present": false,
      "lease": "active",
      "estimated_tokens": 800
    }
  ],
  "expected_violation_codes": []
}"#
}

#[derive(Clone)]
struct AgentStudioForm {
    id: Entry,
    name: Entry,
    description: Entry,
    instructions: Entry,
    tools: Entry,
    max_steps: Entry,
    max_context: Entry,
    max_output: Entry,
    timeout: Entry,
    daily_tokens: Entry,
    daily_cost_microusd: Entry,
    input_cost_microusd: Entry,
    output_cost_microusd: Entry,
    capability_roots: Entry,
    capability_origins: Entry,
    capability_apps: Entry,
    capability_workspaces: Entry,
    capability_services: Entry,
    capability_secrets: Entry,
    capability_contexts: Entry,
    capability_lease: Entry,
    voice: Switch,
    memory: Switch,
    trigger_id: Entry,
    trigger_kind: ChoiceDropDown,
    trigger_value: Entry,
    trigger_objective: Entry,
    trigger_interval: Entry,
    trigger_cooldown: Entry,
    trigger_hourly: Entry,
    trigger_enabled: CheckButton,
}

fn studio_entry(page: &Box, label: &str, initial: &str) -> Entry {
    let row = Box::new(Orientation::Horizontal, 8);
    let caption = Label::new(Some(label));
    caption.set_xalign(0.0);
    caption.set_width_chars(20);
    let entry = Entry::new();
    entry.set_hexpand(true);
    entry.set_text(initial);
    row.append(&caption);
    row.append(&entry);
    page.append(&row);
    entry
}

fn studio_switch(page: &Box, label: &str) -> Switch {
    let row = Box::new(Orientation::Horizontal, 8);
    let caption = Label::new(Some(label));
    caption.set_xalign(0.0);
    caption.set_hexpand(true);
    let control = Switch::new();
    row.append(&caption);
    row.append(&control);
    page.append(&row);
    control
}

fn build_agent_studio_page(log_buffer: TextBuffer) -> Box {
    let page = Box::new(Orientation::Vertical, 10);
    page.add_css_class("detail-pane");
    page.set_hexpand(true);

    let title = Label::new(Some("Agent Studio"));
    title.set_xalign(0.0);
    title.add_css_class("page-title");
    page.append(&title);
    let intro = Label::new(Some(
        "Build bounded declarative agents, simulate plans without executing tools, hot-reload packages, inspect health and durable runs, and control every background trigger.",
    ));
    intro.set_xalign(0.0);
    intro.set_wrap(true);
    page.append(&intro);

    let emergency_row = Box::new(Orientation::Horizontal, 8);
    let emergency_label = Label::new(Some("Suspend all scheduled and event-driven agents"));
    emergency_label.set_xalign(0.0);
    emergency_label.set_hexpand(true);
    let emergency = Switch::new();
    emergency.set_sensitive(false);
    emergency_row.append(&emergency_label);
    emergency_row.append(&emergency);
    page.append(&emergency_row);

    let status = Label::new(Some("Loading trigger safety state…"));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("source-status");
    page.append(&status);

    let form = AgentStudioForm {
        id: studio_entry(&page, "Agent ID", "workspace-guide"),
        name: studio_entry(&page, "Name", "Workspace Guide"),
        description: studio_entry(&page, "Description", "Explains the active workspace."),
        instructions: studio_entry(
            &page,
            "Instructions",
            "Use observed desktop metadata only. Clearly label proposed actions.",
        ),
        tools: studio_entry(&page, "Allowed tools", "list_windows,list_workspaces"),
        max_steps: studio_entry(&page, "Maximum tool steps", "2"),
        max_context: studio_entry(&page, "Context characters", "16000"),
        max_output: studio_entry(&page, "Output tokens", "512"),
        timeout: studio_entry(&page, "Deadline seconds", "45"),
        daily_tokens: studio_entry(&page, "Daily token limit", ""),
        daily_cost_microusd: studio_entry(&page, "Daily cost limit (µUSD)", ""),
        input_cost_microusd: studio_entry(&page, "Input µUSD / 1M tokens", ""),
        output_cost_microusd: studio_entry(&page, "Output µUSD / 1M tokens", ""),
        capability_roots: studio_entry(&page, "Filesystem roots", ""),
        capability_origins: studio_entry(&page, "Network origins", ""),
        capability_apps: studio_entry(&page, "Application scope", ""),
        capability_workspaces: studio_entry(&page, "Workspace scope", ""),
        capability_services: studio_entry(&page, "Service scope", ""),
        capability_secrets: studio_entry(&page, "Opaque secret handles", ""),
        capability_contexts: studio_entry(
            &page,
            "Context kinds",
            "active_window,workspace,conversation",
        ),
        capability_lease: studio_entry(&page, "Capability lease seconds", "120"),
        voice: studio_switch(&page, "Voice capable"),
        memory: studio_switch(&page, "Memory enabled"),
        trigger_id: studio_entry(&page, "Trigger ID (optional)", ""),
        trigger_kind: ChoiceDropDown::new(),
        trigger_value: studio_entry(&page, "Trigger match value", "session_started"),
        trigger_objective: studio_entry(
            &page,
            "Trigger objective",
            "Inspect the new session and report anything needing attention.",
        ),
        trigger_interval: studio_entry(&page, "Schedule interval", "300"),
        trigger_cooldown: studio_entry(&page, "Trigger cooldown", "300"),
        trigger_hourly: studio_entry(&page, "Trigger hourly limit", "2"),
        trigger_enabled: CheckButton::with_label("Trigger enabled"),
    };
    form.trigger_kind.replace(
        vec![
            ("desktop_event".into(), "Desktop event".into()),
            ("schedule".into(), "Schedule".into()),
            ("voice_phrase".into(), "Voice phrase".into()),
            ("hotkey".into(), "Hotkey".into()),
            ("ipc_event".into(), "IPC event".into()),
        ],
        "desktop_event",
    );
    let trigger_kind_row = Box::new(Orientation::Horizontal, 8);
    let trigger_kind_label = Label::new(Some("Trigger kind"));
    trigger_kind_label.set_xalign(0.0);
    trigger_kind_label.set_width_chars(20);
    trigger_kind_row.append(&trigger_kind_label);
    trigger_kind_row.append(&form.trigger_kind.widget);
    page.append(&trigger_kind_row);
    form.trigger_enabled.set_active(true);
    page.append(&form.trigger_enabled);

    let update_existing = CheckButton::with_label("Update existing package and create backup");
    page.append(&update_existing);
    let button_row = Box::new(Orientation::Horizontal, 8);
    let validate = Button::with_label("Validate & preview");
    let install = Button::with_label("Install package");
    install.add_css_class("suggested-action");
    let refresh_history = Button::with_label("Refresh run history");
    button_row.append(&validate);
    button_row.append(&install);
    button_row.append(&refresh_history);
    page.append(&button_row);

    let managed_agent = ChoiceDropDown::new();
    let managed_agent_row = Box::new(Orientation::Horizontal, 8);
    let managed_agent_label = Label::new(Some("Installed agent"));
    managed_agent_label.set_xalign(0.0);
    managed_agent_label.set_width_chars(20);
    managed_agent_row.append(&managed_agent_label);
    managed_agent.widget.set_hexpand(true);
    managed_agent_row.append(&managed_agent.widget);
    page.append(&managed_agent_row);

    let control_row = Box::new(Orientation::Horizontal, 8);
    let reload_agents = Button::with_label("Hot reload");
    let enable_agent = Button::with_label("Enable");
    let disable_agent = Button::with_label("Disable");
    let rollback_agent = Button::with_label("Roll back");
    let refresh_control = Button::with_label("Refresh health");
    control_row.append(&reload_agents);
    control_row.append(&enable_agent);
    control_row.append(&disable_agent);
    control_row.append(&rollback_agent);
    control_row.append(&refresh_control);
    page.append(&control_row);

    let capability_row = Box::new(Orientation::Horizontal, 8);
    let preview_capabilities = Button::with_label("Preview authority");
    let list_capability_leases = Button::with_label("Active leases");
    let revoke_capability = Button::with_label("Revoke lease");
    capability_row.append(&preview_capabilities);
    capability_row.append(&list_capability_leases);
    capability_row.append(&revoke_capability);
    page.append(&capability_row);
    let capability_lease_id = studio_entry(&page, "Lease ID to revoke", "");

    let dry_run_objective = studio_entry(
        &page,
        "Dry-run objective",
        "Inspect the current desktop and explain the tools you would use.",
    );
    let dry_run = Button::with_label("Simulate plan (no tools execute)");
    page.append(&dry_run);

    let control_buffer = TextBuffer::new(None);
    control_buffer.set_text("Select Refresh health to inspect the live agent registry.");
    let control_output = TextView::with_buffer(&control_buffer);
    control_output.set_editable(false);
    control_output.set_monospace(true);
    control_output.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&control_output)
            .height_request(220)
            .hexpand(true)
            .build(),
    );

    let workflow_id = studio_entry(&page, "Workflow ID", "workspace-troubleshooter");
    let workflow_run_id = studio_entry(&page, "Workflow run ID", "");
    let workflow_row = Box::new(Orientation::Horizontal, 8);
    let list_workflows = Button::with_label("List workflows");
    let start_workflow = Button::with_label("Start workflow");
    let list_workflow_runs = Button::with_label("Workflow runs");
    let pause_workflow = Button::with_label("Pause");
    let resume_workflow = Button::with_label("Resume");
    let cancel_workflow = Button::with_label("Cancel");
    let retry_workflow = Button::with_label("Retry");
    for button in [
        &list_workflows,
        &start_workflow,
        &list_workflow_runs,
        &pause_workflow,
        &resume_workflow,
        &cancel_workflow,
        &retry_workflow,
    ] {
        workflow_row.append(button);
    }
    page.append(&workflow_row);

    let preview_buffer = TextBuffer::new(None);
    preview_buffer.set_text("Validated manifest TOML will appear here.");
    let preview = TextView::with_buffer(&preview_buffer);
    preview.set_editable(false);
    preview.set_monospace(true);
    preview.set_wrap_mode(gtk4::WrapMode::WordChar);
    let preview_scroll = ScrolledWindow::builder()
        .child(&preview)
        .height_request(220)
        .hexpand(true)
        .build();
    page.append(&preview_scroll);

    let history_search = studio_entry(&page, "History search", "");
    let history_buffer = TextBuffer::new(None);
    history_buffer.set_text("Select Refresh run history to inspect durable runs.");
    let history = TextView::with_buffer(&history_buffer);
    history.set_editable(false);
    history.set_monospace(true);
    history.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&history)
            .height_request(220)
            .hexpand(true)
            .build(),
    );

    {
        let form = form.clone();
        let preview_buffer = preview_buffer.clone();
        let status = status.clone();
        validate.connect_clicked(move |_| match agent_studio_definition(&form) {
            Ok(definition) => match definition.to_toml() {
                Ok(toml) => {
                    preview_buffer.set_text(&toml);
                    status.set_text("Manifest is valid and bounded.");
                }
                Err(error) => status.set_text(&format!("Manifest encoding failed: {error}")),
            },
            Err(error) => status.set_text(&format!("Manifest validation failed: {error}")),
        });
    }

    {
        let form = form.clone();
        let status = status.clone();
        let preview_buffer = preview_buffer.clone();
        let update_existing = update_existing.clone();
        let log_buffer = log_buffer.clone();
        install.connect_clicked(move |_| {
            let definition = match agent_studio_definition(&form) {
                Ok(definition) => definition,
                Err(error) => {
                    status.set_text(&format!("Install blocked: {error}"));
                    return;
                }
            };
            preview_buffer.set_text(&definition.to_toml().unwrap_or_default());
            append_log(
                &log_buffer,
                &format!("[agent-studio] requested install for {}", definition.id),
            );
            dispatch_agent_studio_request(
                AiIpcRequest::InstallAgent {
                    definition: std::boxed::Box::new(definition),
                    overwrite: update_existing.is_active(),
                },
                status.clone(),
                preview_buffer.clone(),
            );
        });
    }

    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        reload_agents.connect_clicked(move |_| {
            dispatch_agent_studio_request(
                AiIpcRequest::ReloadAgents,
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        let agent_id = form.id.clone();
        preview_capabilities.connect_clicked(move |_| {
            dispatch_agent_studio_request(
                AiIpcRequest::PreviewCapabilities {
                    agent_id: agent_id.text().trim().to_string(),
                    ceiling: None,
                },
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        list_capability_leases.connect_clicked(move |_| {
            dispatch_agent_studio_request(
                AiIpcRequest::ListCapabilityLeases,
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        revoke_capability.connect_clicked(move |_| {
            dispatch_agent_studio_request(
                AiIpcRequest::RevokeCapabilityLease {
                    lease_id: capability_lease_id.text().trim().to_string(),
                },
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    for (button, definitions) in [(list_workflows, true), (list_workflow_runs, false)] {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        button.connect_clicked(move |_| {
            dispatch_agent_studio_request(
                if definitions {
                    AiIpcRequest::ListWorkflows
                } else {
                    AiIpcRequest::ListWorkflowRuns
                },
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        let workflow_id = workflow_id.clone();
        start_workflow.connect_clicked(move |_| {
            dispatch_agent_studio_request(
                AiIpcRequest::StartWorkflow {
                    workflow_id: workflow_id.text().trim().to_string(),
                },
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    for (button, action) in [
        (pause_workflow, "pause"),
        (resume_workflow, "resume"),
        (cancel_workflow, "cancel"),
        (retry_workflow, "retry"),
    ] {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        let run_id = workflow_run_id.clone();
        button.connect_clicked(move |_| {
            let run_id = run_id.text().trim().to_string();
            let request = match action {
                "pause" => AiIpcRequest::SetWorkflowPaused {
                    run_id,
                    paused: true,
                },
                "resume" => AiIpcRequest::SetWorkflowPaused {
                    run_id,
                    paused: false,
                },
                "cancel" => AiIpcRequest::CancelWorkflow { run_id },
                _ => AiIpcRequest::RetryWorkflow { run_id },
            };
            dispatch_agent_studio_request(request, status.clone(), control_buffer.clone());
        });
    }
    for (button, enabled) in [(enable_agent, true), (disable_agent, false)] {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        let agent_id = managed_agent.clone();
        button.connect_clicked(move |_| {
            let Some(agent_id) = agent_id.selected_id() else {
                status.set_text("Refresh health and select an installed agent first.");
                return;
            };
            dispatch_agent_studio_request(
                AiIpcRequest::SetAgentEnabled { agent_id, enabled },
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        let agent_id = managed_agent.clone();
        rollback_agent.connect_clicked(move |_| {
            let Some(agent_id) = agent_id.selected_id() else {
                status.set_text("Refresh health and select an installed agent first.");
                return;
            };
            dispatch_agent_studio_request(
                AiIpcRequest::RollbackAgent { agent_id },
                status.clone(),
                control_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        let managed_agent = managed_agent.clone();
        refresh_control.connect_clicked(move |_| {
            dispatch_managed_agent_refresh(
                status.clone(),
                control_buffer.clone(),
                managed_agent.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let control_buffer = control_buffer.clone();
        let agent_id = form.id.clone();
        dry_run.connect_clicked(move |_| {
            dispatch_agent_studio_request(
                AiIpcRequest::DryRunAgent {
                    request: AgentRequest {
                        objective: dry_run_objective.text().trim().to_string(),
                        agent_id: Some(agent_id.text().trim().to_string()),
                        provider: None,
                        model: None,
                    },
                },
                status.clone(),
                control_buffer.clone(),
            );
        });
    }

    {
        let status = status.clone();
        let history_buffer = history_buffer.clone();
        let history_search = history_search.clone();
        refresh_history.connect_clicked(move |_| {
            let (tx, rx) = mpsc::channel::<Result<Vec<AgentRunStatus>, String>>();
            thread::spawn(move || {
                let result = match send_ai_request(&AiIpcRequest::ListAgentRuns) {
                    Ok(AiIpcResponse::AgentRuns { runs }) => Ok(runs),
                    Ok(AiIpcResponse::Error { message }) => Err(message),
                    Ok(other) => Err(format!("unexpected AI response: {other:?}")),
                    Err(error) => Err(error.to_string()),
                };
                let _ = tx.send(result);
            });
            let status = status.clone();
            let history_buffer = history_buffer.clone();
            let query = history_search.text().trim().to_lowercase();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(runs)) => {
                    let rendered = render_agent_history(&runs, &query);
                    history_buffer.set_text(&rendered);
                    status.set_text(&format!("Loaded {} retained runs.", runs.len()));
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    status.set_text(&format!("History refresh failed: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
            });
        });
    }

    let syncing_emergency = Rc::new(Cell::new(true));
    {
        let (tx, rx) = mpsc::channel::<Result<bool, String>>();
        thread::spawn(move || {
            let result = match send_ai_request(&AiIpcRequest::GetAgentTriggerState) {
                Ok(AiIpcResponse::AgentTriggerState { suspended }) => Ok(suspended),
                Ok(AiIpcResponse::Error { message }) => Err(message),
                Ok(other) => Err(format!("unexpected AI response: {other:?}")),
                Err(error) => Err(error.to_string()),
            };
            let _ = tx.send(result);
        });
        let emergency = emergency.clone();
        let status = status.clone();
        let syncing = syncing_emergency.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
            Ok(Ok(suspended)) => {
                emergency.set_active(suspended);
                emergency.set_sensitive(true);
                syncing.set(false);
                status.set_text(if suspended {
                    "Emergency suspension is active. Manual agent runs remain available."
                } else {
                    "Agent triggers are active."
                });
                ControlFlow::Break
            }
            Ok(Err(error)) => {
                status.set_text(&format!("Trigger state unavailable: {error}"));
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
        });
    }
    {
        let syncing = syncing_emergency.clone();
        let status = status.clone();
        emergency.connect_active_notify(move |control| {
            if syncing.get() {
                return;
            }
            let suspended = control.is_active();
            let (tx, rx) = mpsc::channel::<Result<bool, String>>();
            thread::spawn(move || {
                let result =
                    match send_ai_request(&AiIpcRequest::SetAgentTriggersSuspended { suspended }) {
                        Ok(AiIpcResponse::AgentTriggerState { suspended }) => Ok(suspended),
                        Ok(AiIpcResponse::Error { message }) => Err(message),
                        Ok(other) => Err(format!("unexpected AI response: {other:?}")),
                        Err(error) => Err(error.to_string()),
                    };
                let _ = tx.send(result);
            });
            let status = status.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(true)) => {
                    status.set_text("All scheduled and event-driven agents are suspended.");
                    ControlFlow::Break
                }
                Ok(Ok(false)) => {
                    status.set_text("Agent triggers are active.");
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    status.set_text(&format!("Trigger safety update failed: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
            });
        });
    }

    page
}

fn dispatch_agent_studio_request(request: AiIpcRequest, status: Label, output: TextBuffer) {
    status.set_text("Agent control request in progress…");
    let (tx, rx) = mpsc::channel::<Result<AiIpcResponse, String>>();
    thread::spawn(move || {
        let result = send_ai_request(&request).map_err(|error| error.to_string());
        let _ = tx.send(result);
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::Agents { agents })) => {
            output.set_text(
                &agents
                    .iter()
                    .map(|agent| format!("{}  {}", agent.id, agent.name))
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            status.set_text(&format!(
                "Live registry now contains {} agents.",
                agents.len()
            ));
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::AgentControlStatuses { agents })) => {
            output.set_text(&render_managed_agents(&agents));
            status.set_text("Agent health and budget state refreshed.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::AgentDryRun { report })) => {
            let planned = report
                .planned_steps
                .iter()
                .map(|step| {
                    let mutating = report
                        .permitted_tools
                        .iter()
                        .find(|tool| tool.name == step.tool)
                        .is_some_and(|tool| tool.mutating);
                    format!(
                        "{}{} {}",
                        step.tool,
                        if mutating {
                            " [confirmation required]"
                        } else {
                            ""
                        },
                        step.arguments
                    )
                })
                .collect::<Vec<_>>();
            output.set_text(&format!(
                "DRY RUN — no tools executed\nagent={} provider={} model={}\nlimits: steps={} context={} output={}\nplan:\n{}\nanswer: {}\ntokens: {}",
                report.agent_id,
                report.provider,
                report.model.as_deref().unwrap_or("default"),
                report.max_tool_steps,
                report.max_context_chars,
                report.max_output_tokens,
                if planned.is_empty() { "(no tools)".into() } else { planned.join("\n") },
                report.answer.as_deref().unwrap_or("(plan requires observations)"),
                report
                    .usage
                    .map(|usage| usage.input_tokens.saturating_add(usage.output_tokens))
                    .unwrap_or(0),
            ));
            status.set_text("Dry run completed without executing tools.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Workflows { workflows })) => {
            output.set_text(
                &workflows
                    .iter()
                    .map(|workflow| {
                        let edges = workflow
                            .nodes
                            .iter()
                            .map(|node| {
                                format!(
                                    "  {} -> {} [{}]",
                                    if node.depends_on.is_empty() {
                                        "start".into()
                                    } else {
                                        node.depends_on.join(",")
                                    },
                                    node.id,
                                    node.agent_id
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        format!(
                            "{} — {}\nparallel={} deadline={}s tokens={}\n{}",
                            workflow.id,
                            workflow.name,
                            workflow.max_parallelism,
                            workflow.timeout_seconds,
                            workflow.max_total_tokens,
                            edges
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
            status.set_text("Loaded bounded workflow definitions.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::WorkflowStarted { run_id })) => {
            output.set_text(&format!("Workflow started.\nrun_id={run_id}"));
            status.set_text("Workflow supervisor is running.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::WorkflowRuns { runs })) => {
            output.set_text(
                &runs
                    .iter()
                    .map(render_workflow_run)
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            );
            status.set_text(&format!("Loaded {} workflow runs.", runs.len()));
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::WorkflowRun { status: run, .. })) => {
            output.set_text(
                &run.as_ref()
                    .map(render_workflow_run)
                    .unwrap_or_else(|| "Unknown workflow run.".into()),
            );
            status.set_text("Workflow status refreshed.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::WorkflowControl { run_id, accepted })) => {
            output.set_text(&format!(
                "Workflow control {} for {run_id}.",
                if accepted {
                    "accepted"
                } else {
                    "made no change"
                }
            ));
            status.set_text("Workflow lifecycle updated.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::CapabilityPreview { preview })) => {
            output.set_text(&format!(
                "agent={}\nlease={}s\ntools={}\nscopes={}",
                preview.agent_id,
                preview.lease_seconds,
                preview.tools.join(", "),
                serde_json::to_string_pretty(&preview.policy).unwrap_or_default()
            ));
            status.set_text("Effective authority preview loaded.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::CapabilityLeases { leases })) => {
            output.set_text(
                &leases
                    .iter()
                    .map(|lease| {
                        format!(
                            "{}  agent={} run={} expires={} {} workflow={}",
                            lease.lease_id,
                            lease.agent_id,
                            lease.run_id,
                            lease.expires_at_unix,
                            if lease.revoked { "revoked" } else { "active" },
                            lease.workflow_run_id.as_deref().unwrap_or("-")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
            status.set_text("Capability lease map refreshed.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::CapabilityRevocation { lease_id, revoked })) => {
            output.set_text(&format!(
                "Lease {lease_id}: {}",
                if revoked {
                    "revoked"
                } else {
                    "already inactive or unknown"
                }
            ));
            status.set_text("Capability revocation processed.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            status.set_text(&format!("Agent control request failed: {message}"));
            ControlFlow::Break
        }
        Ok(Ok(other)) => {
            status.set_text(&format!("Unexpected agent control response: {other:?}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Agent control request disconnected.");
            ControlFlow::Break
        }
    });
}

fn dispatch_managed_agent_refresh(status: Label, output: TextBuffer, selector: ChoiceDropDown) {
    status.set_text("Loading installed agents and declared authority…");
    let (tx, rx) = mpsc::channel::<Result<Vec<focaldesk_ai::AgentControlStatus>, String>>();
    thread::spawn(move || {
        let result = match send_ai_request(&AiIpcRequest::GetAgentControlStatuses) {
            Ok(AiIpcResponse::AgentControlStatuses { agents }) => Ok(agents),
            Ok(AiIpcResponse::Error { message }) => Err(message),
            Ok(other) => Err(format!("unexpected AI response: {other:?}")),
            Err(error) => Err(error.to_string()),
        };
        let _ = tx.send(result);
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(agents)) => {
            let choices = agents
                .iter()
                .map(|agent| {
                    (
                        agent.definition.id.clone(),
                        format!("{} ({})", agent.definition.name, agent.definition.id),
                    )
                })
                .collect::<Vec<_>>();
            let selected = selector
                .selected_id()
                .filter(|selected| choices.iter().any(|(id, _)| id == selected))
                .or_else(|| choices.first().map(|(id, _)| id.clone()))
                .unwrap_or_default();
            selector.replace(choices, &selected);
            output.set_text(&render_managed_agents(&agents));
            status.set_text(&format!(
                "Loaded {} installed agents. Select one above to enable, disable, or roll back.",
                agents.len()
            ));
            ControlFlow::Break
        }
        Ok(Err(error)) => {
            status.set_text(&format!("Agent refresh failed: {error}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Agent refresh channel disconnected.");
            ControlFlow::Break
        }
    });
}

fn render_managed_agents(agents: &[focaldesk_ai::AgentControlStatus]) -> String {
    agents
        .iter()
        .map(|agent| {
            let definition = &agent.definition;
            let trigger_summary = if definition.triggers.is_empty() {
                "none".to_string()
            } else {
                definition
                    .triggers
                    .iter()
                    .map(|trigger| format!("{} ({:?}, enabled={})", trigger.id, trigger.kind, trigger.enabled))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let policy = definition
                .capability_policy
                .as_ref()
                .map(|policy| serde_json::to_string_pretty(policy).unwrap_or_default())
                .unwrap_or_else(|| "No additional capability scopes declared.".into());
            format!(
                "{} — {}  [{}]\n{}\nsource={}  voice={}  memory={}\ntools: {}\nlimits: {} steps, {} context characters, {} output tokens, {}s\ntriggers: {}\nusage today: runs={} failures={} tokens={} estimated cost=${:.6}\ncapability scopes:\n{}",
                definition.name,
                definition.id,
                if agent.enabled { "enabled" } else { "disabled" },
                definition.description,
                if definition.built_in { "built-in" } else { "installed" },
                definition.voice,
                definition.memory,
                if definition.tool_allowlist.is_empty() { "none".into() } else { definition.tool_allowlist.join(", ") },
                definition.max_tool_steps,
                definition.max_context_chars,
                definition.max_output_tokens,
                definition.timeout_seconds,
                trigger_summary,
                agent.runs_today,
                agent.failures_today,
                agent.input_tokens_today.saturating_add(agent.output_tokens_today),
                agent.estimated_cost_microusd_today as f64 / 1_000_000.0,
                policy,
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n────────────────────────\n\n")
}

fn render_workflow_run(run: &focaldesk_ai::WorkflowRunStatus) -> String {
    let nodes = run
        .nodes
        .values()
        .map(|node| {
            format!(
                "  {}  {:?}  child={}",
                node.node_id,
                node.state,
                node.agent_run_id.as_deref().unwrap_or("-")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "{}  workflow={}  {:?}  tokens={}/{}\n{}{}",
        run.run_id,
        run.workflow_id,
        run.state,
        run.total_tokens,
        run.max_total_tokens,
        nodes,
        run.error
            .as_ref()
            .map(|error| format!("\nerror: {error}"))
            .unwrap_or_default()
    )
}

fn agent_studio_definition(form: &AgentStudioForm) -> Result<AgentDefinition, String> {
    let parse_usize = |entry: &Entry, name: &str| {
        entry
            .text()
            .parse::<usize>()
            .map_err(|_| format!("{name} must be an integer"))
    };
    let parse_u32 = |entry: &Entry, name: &str| {
        entry
            .text()
            .parse::<u32>()
            .map_err(|_| format!("{name} must be an integer"))
    };
    let parse_u64 = |entry: &Entry, name: &str| {
        entry
            .text()
            .parse::<u64>()
            .map_err(|_| format!("{name} must be an integer"))
    };
    let parse_optional_u64 = |entry: &Entry, name: &str| {
        let value = entry.text();
        if value.trim().is_empty() {
            Ok(None)
        } else {
            value
                .parse::<u64>()
                .map(Some)
                .map_err(|_| format!("{name} must be an integer or blank"))
        }
    };
    let tools = form
        .tools
        .text()
        .split(',')
        .map(str::trim)
        .filter(|tool| !tool.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let scoped_values = |entry: &Entry| {
        let text = entry.text();
        if text.trim().is_empty() {
            None
        } else if text.trim() == "-" {
            Some(Vec::new())
        } else {
            Some(
                text.split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect(),
            )
        }
    };
    let capability_policy = CapabilityPolicy {
        filesystem_roots: scoped_values(&form.capability_roots)
            .map(|roots| roots.into_iter().map(PathBuf::from).collect()),
        network_origins: scoped_values(&form.capability_origins),
        applications: scoped_values(&form.capability_apps),
        workspaces: scoped_values(&form.capability_workspaces),
        services: scoped_values(&form.capability_services),
        secret_handles: scoped_values(&form.capability_secrets),
        context_kinds: scoped_values(&form.capability_contexts)
            .map(|kinds| {
                kinds
                    .into_iter()
                    .map(|kind| match kind.as_str() {
                        "active_window" => Ok(focaldesk_ai::ContextKind::ActiveWindow),
                        "workspace" => Ok(focaldesk_ai::ContextKind::Workspace),
                        "notifications" => Ok(focaldesk_ai::ContextKind::Notifications),
                        "calendar" => Ok(focaldesk_ai::ContextKind::Calendar),
                        "files" => Ok(focaldesk_ai::ContextKind::Files),
                        "conversation" => Ok(focaldesk_ai::ContextKind::Conversation),
                        _ => Err(format!("unknown context kind: {kind}")),
                    })
                    .collect::<Result<Vec<_>, String>>()
            })
            .transpose()?,
        lease_seconds: parse_u64(&form.capability_lease, "capability lease seconds")?,
    };
    let mut builder = AgentBuilder::new(form.id.text(), form.name.text())
        .description(form.description.text())
        .instructions(form.instructions.text())
        .allow_tools(tools)
        .max_tool_steps(parse_usize(&form.max_steps, "maximum tool steps")?)
        .max_context_chars(parse_usize(&form.max_context, "context characters")?)
        .max_output_tokens(parse_u32(&form.max_output, "output tokens")?)
        .timeout_seconds(parse_u64(&form.timeout, "deadline seconds")?)
        .daily_token_limit(parse_optional_u64(&form.daily_tokens, "daily token limit")?)
        .daily_cost_budget(
            parse_optional_u64(&form.daily_cost_microusd, "daily cost limit")?,
            parse_optional_u64(&form.input_cost_microusd, "input price")?,
            parse_optional_u64(&form.output_cost_microusd, "output price")?,
        )
        .capability_policy(Some(capability_policy))
        .voice(form.voice.is_active())
        .memory(form.memory.is_active());

    if !form.trigger_id.text().trim().is_empty() {
        let kind = match form.trigger_kind.selected_id().as_deref() {
            Some("schedule") => AgentTriggerKind::Schedule,
            Some("desktop_event") => AgentTriggerKind::DesktopEvent,
            Some("voice_phrase") => AgentTriggerKind::VoicePhrase,
            Some("hotkey") => AgentTriggerKind::Hotkey,
            Some("ipc_event") => AgentTriggerKind::IpcEvent,
            _ => return Err("select a trigger kind".into()),
        };
        builder = builder.trigger(AgentTrigger {
            id: form.trigger_id.text().to_string(),
            kind,
            objective: form.trigger_objective.text().to_string(),
            match_value: if kind == AgentTriggerKind::Schedule {
                String::new()
            } else {
                form.trigger_value.text().to_string()
            },
            interval_seconds: if kind == AgentTriggerKind::Schedule {
                Some(parse_u64(&form.trigger_interval, "schedule interval")?)
            } else {
                None
            },
            cooldown_seconds: parse_u64(&form.trigger_cooldown, "trigger cooldown")?,
            max_runs_per_hour: parse_usize(&form.trigger_hourly, "trigger hourly limit")?,
            enabled: form.trigger_enabled.is_active(),
        });
    }
    builder.build().map_err(|error| error.to_string())
}

fn render_agent_history(runs: &[AgentRunStatus], query: &str) -> String {
    let mut lines = Vec::new();
    for run in runs {
        let trigger = run
            .trigger
            .as_ref()
            .map(|source| format!("{}:{}", source.kind.as_str(), source.trigger_id))
            .unwrap_or_else(|| "manual".into());
        let line = format!(
            "{}  {}  agent={}  source={}  steps={}/{}  tokens={}  {}",
            run.run_id,
            run.state.as_str(),
            run.agent_id,
            trigger,
            run.completed_tool_steps,
            run.max_tool_steps,
            run.result
                .as_ref()
                .and_then(|response| response.usage)
                .map(|usage| usage.input_tokens.saturating_add(usage.output_tokens))
                .unwrap_or(0),
            run.objective_preview
        );
        if query.is_empty() || line.to_lowercase().contains(query) {
            lines.push(line);
        }
    }
    if lines.is_empty() {
        "No retained runs match the search.".into()
    } else {
        lines.join("\n")
    }
}

fn build_desktop_agent_page(
    state: Rc<RefCell<PersistedState>>,
    runtime: Rc<RefCell<AiConsoleRuntime>>,
    log_buffer: TextBuffer,
) -> Rc<DesktopAgentPage> {
    let page = Box::new(Orientation::Vertical, 12);
    page.add_css_class("detail-pane");
    page.set_hexpand(true);
    page.set_vexpand(true);

    let title = Label::new(Some("Desktop Agent"));
    title.set_xalign(0.0);
    title.add_css_class("page-title");
    page.append(&title);

    let description = Label::new(Some(
        "Ask FocalDesk to inspect bounded desktop metadata and propose supported actions. Read-only steps run after permission approval; every mutation requires a separate native one-shot confirmation.",
    ));
    description.set_xalign(0.0);
    description.set_wrap(true);
    description.add_css_class("mode-banner-body");
    page.append(&description);

    let profile_row = Box::new(Orientation::Horizontal, 8);
    let profile_label = Label::new(Some("Agent profile"));
    profile_label.set_xalign(0.0);
    let agent_profile = ChoiceDropDown::new();
    agent_profile.replace(
        vec![
            ("desktop".into(), "Desktop Assistant".into()),
            ("troubleshooter".into(), "System Troubleshooter".into()),
            ("accessibility".into(), "Accessibility Assistant".into()),
        ],
        "desktop",
    );
    profile_row.append(&profile_label);
    profile_row.append(&agent_profile.widget);
    page.append(&profile_row);

    let objective_row = Box::new(Orientation::Horizontal, 8);
    let objective = Entry::builder()
        .placeholder_text("Inspect my desktop and explain what needs attention")
        .hexpand(true)
        .build();
    objective.set_max_length(4_000);
    let voice_objective = Button::with_label("Voice objective");
    connect_continuous_dictation(&voice_objective, &objective, &log_buffer, "agent-voice");
    let run_button = Button::with_label("Run agent");
    run_button.add_css_class("suggested-action");
    objective_row.append(&objective);
    objective_row.append(&voice_objective);
    objective_row.append(&run_button);
    page.append(&objective_row);

    let status = Label::new(Some("Ready · no desktop data has been shared"));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("source-status");
    page.append(&status);

    let result_buffer = TextBuffer::new(None);
    result_buffer.set_text(
        "Agent results will appear here, including every tool call and any proposed action.",
    );
    let result_view = TextView::with_buffer(&result_buffer);
    result_view.set_editable(false);
    result_view.set_cursor_visible(false);
    result_view.set_monospace(true);
    result_view.set_wrap_mode(gtk4::WrapMode::WordChar);
    result_view.add_css_class("agent-result");
    let result_scroll = ScrolledWindow::builder()
        .child(&result_view)
        .hexpand(true)
        .vexpand(true)
        .build();
    result_scroll.add_css_class("pane-scroll");
    page.append(&result_scroll);

    let action_row = Box::new(Orientation::Horizontal, 8);
    let cancel_button = Button::with_label("Cancel run");
    cancel_button.set_sensitive(false);
    let approve_button = Button::with_label("Approve once");
    approve_button.add_css_class("suggested-action");
    approve_button.set_sensitive(false);
    let deny_button = Button::with_label("Deny");
    deny_button.set_sensitive(false);
    let read_proposal_button = Button::with_label("Read proposal aloud");
    {
        let result_buffer = result_buffer.clone();
        let log_buffer = log_buffer.clone();
        read_proposal_button.connect_clicked(move |_| {
            let text = result_buffer.text(
                &result_buffer.start_iter(),
                &result_buffer.end_iter(),
                false,
            );
            speak_local_async(text.to_string(), log_buffer.clone());
        });
    }
    action_row.append(&cancel_button);
    action_row.append(&read_proposal_button);
    action_row.append(&approve_button);
    action_row.append(&deny_button);
    page.append(&action_row);

    let handles = Rc::new(DesktopAgentPage {
        page,
        agent_profile,
        objective,
        status,
        result_buffer,
        run_button,
        cancel_button,
        approve_button,
        deny_button,
        pending_plan_id: Rc::new(RefCell::new(None)),
        active_run_id: Rc::new(RefCell::new(None)),
    });

    {
        let (tx, rx) = mpsc::channel::<Result<Vec<(String, String)>, String>>();
        thread::spawn(move || {
            let result = match send_ai_request(&AiIpcRequest::ListAgents) {
                Ok(AiIpcResponse::Agents { agents }) => Ok(agents
                    .into_iter()
                    .map(|agent| (agent.id, agent.name))
                    .collect()),
                Ok(AiIpcResponse::Error { message }) => Err(message),
                Ok(other) => Err(format!("unexpected AI response: {other:?}")),
                Err(error) => Err(error.to_string()),
            };
            let _ = tx.send(result);
        });
        let profile = handles.agent_profile.clone();
        let status = handles.status.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
            Ok(Ok(agents)) => {
                profile.replace(agents, "desktop");
                ControlFlow::Break
            }
            Ok(Err(message)) => {
                status.set_text(&format!("Agent profiles unavailable: {message}"));
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
        });
    }

    {
        let handles = handles.clone();
        let log_buffer = log_buffer.clone();
        handles.cancel_button.clone().connect_clicked(move |_| {
            let Some(run_id) = handles.active_run_id.borrow().clone() else {
                return;
            };
            handles.cancel_button.set_sensitive(false);
            let (tx, rx) = mpsc::channel::<Result<bool, String>>();
            thread::spawn(move || {
                let result = match send_ai_request(&AiIpcRequest::CancelAgentRun { run_id }) {
                    Ok(AiIpcResponse::AgentRunCancellation { accepted, .. }) => Ok(accepted),
                    Ok(AiIpcResponse::Error { message }) => Err(message),
                    Ok(other) => Err(format!("unexpected AI response: {other:?}")),
                    Err(error) => Err(error.to_string()),
                };
                let _ = tx.send(result);
            });
            let handles = handles.clone();
            let log_buffer = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(true)) => {
                    handles.status.set_text("Agent run cancelled");
                    append_log(&log_buffer, "[agent] run cancelled");
                    ControlFlow::Break
                }
                Ok(Ok(false)) => {
                    handles
                        .status
                        .set_text("Agent run was no longer cancellable");
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    handles
                        .status
                        .set_text(&format!("Cancellation failed: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
            });
        });
    }
    {
        let handles = handles.clone();
        let state = state.clone();
        let runtime = runtime.clone();
        let log_buffer = log_buffer.clone();
        handles.run_button.clone().connect_clicked(move |_| {
            dispatch_desktop_agent(
                handles.clone(),
                state.clone(),
                runtime.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let handles = handles.clone();
        let state = state.clone();
        let runtime = runtime.clone();
        let log_buffer = log_buffer.clone();
        handles.objective.clone().connect_activate(move |_| {
            dispatch_desktop_agent(
                handles.clone(),
                state.clone(),
                runtime.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let handles = handles.clone();
        let log_buffer = log_buffer.clone();
        handles.approve_button.clone().connect_clicked(move |_| {
            resolve_desktop_agent_action(handles.clone(), true, log_buffer.clone());
        });
    }
    {
        let handles = handles.clone();
        handles.deny_button.clone().connect_clicked(move |_| {
            resolve_desktop_agent_action(handles.clone(), false, log_buffer.clone());
        });
    }

    handles
}

fn connect_continuous_dictation(
    button: &Button,
    entry: &Entry,
    log_buffer: &TextBuffer,
    log_target: &'static str,
) {
    let current_session: Rc<RefCell<Option<AmbientMicClient>>> = Rc::new(RefCell::new(None));
    let entry = entry.clone();
    let log_buffer = log_buffer.clone();
    button.connect_clicked(move |button| {
        if let Some(session) = current_session.borrow().as_ref() {
            session.stop();
            button.set_label("Voice objective");
            return;
        }
        let (session, rx) = match start_daemon_dictation("focaldesk-ai-console/agent") {
            Ok(session) => session,
            Err(error) => {
                append_log(&log_buffer, &format!("[{log_target}] {error}"));
                return;
            }
        };
        *current_session.borrow_mut() = Some(session);
        button.set_label("Stop listening");

        let mut base = entry.text().trim_end().to_string();
        if !base.is_empty() {
            base.push(' ');
        }
        let mut accumulated = String::new();
        let entry_for_poll = entry.clone();
        let button_for_poll = button.clone();
        let log_for_poll = log_buffer.clone();
        let session_for_poll = current_session.clone();
        glib::timeout_add_local(Duration::from_millis(80), move || {
            loop {
                match rx.try_recv() {
                    Ok(VoiceEvent::Ready) => {
                        append_log(&log_for_poll, &format!("[{log_target}] microphone ready"));
                    }
                    Ok(VoiceEvent::Partial(partial)) => {
                        entry_for_poll.set_text(&format!("{base}{accumulated}{partial}"));
                        entry_for_poll.set_position(-1);
                    }
                    Ok(VoiceEvent::Final(text)) => {
                        if !text.trim().is_empty() {
                            accumulated.push_str(text.trim());
                            accumulated.push(' ');
                        }
                        entry_for_poll.set_text(&format!("{base}{accumulated}"));
                        entry_for_poll.set_position(-1);
                    }
                    Ok(VoiceEvent::Stopped) => {
                        button_for_poll.set_label("Voice objective");
                        *session_for_poll.borrow_mut() = None;
                        return ControlFlow::Break;
                    }
                    Ok(
                        VoiceEvent::VoiceActivity(_)
                        | VoiceEvent::WakeDetected
                        | VoiceEvent::Command(_),
                    ) => {}
                    Ok(VoiceEvent::Error(error)) => {
                        append_log(&log_for_poll, &format!("[{log_target}] {error}"));
                        button_for_poll.set_label("Voice objective");
                        *session_for_poll.borrow_mut() = None;
                        return ControlFlow::Break;
                    }
                    Err(mpsc::TryRecvError::Empty) => return ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        button_for_poll.set_label("Voice objective");
                        *session_for_poll.borrow_mut() = None;
                        return ControlFlow::Break;
                    }
                }
            }
        });
    });
}

fn build_context_fabric_page(state: Rc<RefCell<PersistedState>>, log_buffer: TextBuffer) -> Box {
    let page = section_shell(
        "Context Fabric",
        "Publish bounded, expiring context and explicitly grant named agents access. No screen pixels, clipboard data, or secrets are collected here.",
    );
    page.append(&info_card(&[
        "Every envelope carries provenance, sensitivity, and an expiry time.".into(),
        "Agents receive context only when both a live grant and their capability policy allow its kind.".into(),
        "Payload text is labeled untrusted evidence and cannot authorize an action.".into(),
    ]));

    let status = Label::new(Some("No context has been shared in this Console session."));
    status.set_xalign(0.0);
    status.set_wrap(true);
    page.append(&status);
    let output = TextBuffer::new(None);
    let view = TextView::with_buffer(&output);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&view)
            .height_request(240)
            .hexpand(true)
            .build(),
    );

    let publish_row = Box::new(Orientation::Horizontal, 8);
    let publish_desktop = Button::with_label("Share active desktop metadata");
    let publish_conversation = Button::with_label("Share active conversation");
    let refresh = Button::with_label("Refresh inspector");
    publish_row.append(&publish_desktop);
    publish_row.append(&publish_conversation);
    publish_row.append(&refresh);
    page.append(&publish_row);

    let agent_id = studio_entry(&page, "Grant to agent", "desktop");
    let kinds = studio_entry(
        &page,
        "Context kinds",
        "active_window,workspace,conversation",
    );
    let ttl = studio_entry(&page, "Grant TTL seconds", "300");
    let grant = Button::with_label("Grant expiring access");
    grant.add_css_class("suggested-action");
    page.append(&grant);
    let revoke_id = studio_entry(&page, "Grant ID to revoke", "");
    let controls = Box::new(Orientation::Horizontal, 8);
    let revoke = Button::with_label("Revoke grant");
    let clear = Button::with_label("Clear all context");
    clear.add_css_class("destructive-action");
    controls.append(&revoke);
    controls.append(&clear);
    page.append(&controls);
    let suggestion_id = studio_entry(&page, "Suggestion ID", "");
    let dismiss_suggestion = Button::with_label("Dismiss suggestion");
    page.append(&dismiss_suggestion);

    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        refresh.connect_clicked(move |_| {
            dispatch_context_request(
                AiIpcRequest::GetContextState,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        publish_desktop.connect_clicked(move |_| {
            status.set_text("Reading the compositor's typed metadata snapshot…");
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let result =
                    send_desktop_request(&IpcRequest::GetDesktopSnapshot).and_then(|response| {
                        match response {
                            IpcResponse::DesktopSnapshot { snapshot } => {
                                let focused_window = snapshot.windows.iter().find(|window| {
                                    Some(window.id) == snapshot.session.focused_window_id
                                });
                                let active_workspace =
                                    snapshot.workspaces.iter().find(|workspace| {
                                        workspace.id == snapshot.session.active_workspace_id
                                    });
                                let window_payload = serde_json::to_value(focused_window)
                                    .map_err(|error| error.to_string())?;
                                let workspace_payload = serde_json::to_value(active_workspace)
                                    .map_err(|error| error.to_string())?;
                                let first = send_ai_request(&AiIpcRequest::PublishContext {
                                    kind: focaldesk_ai::ContextKind::ActiveWindow,
                                    provenance: "compositor desktop snapshot".into(),
                                    sensitivity: focaldesk_ai::ContextSensitivity::Private,
                                    payload: window_payload,
                                    ttl_seconds: 120,
                                })
                                .map_err(|error| error.to_string())?;
                                let second = send_ai_request(&AiIpcRequest::PublishContext {
                                    kind: focaldesk_ai::ContextKind::Workspace,
                                    provenance: "compositor desktop snapshot".into(),
                                    sensitivity: focaldesk_ai::ContextSensitivity::Private,
                                    payload: workspace_payload,
                                    ttl_seconds: 120,
                                })
                                .map_err(|error| error.to_string())?;
                                Ok((first, second))
                            }
                            IpcResponse::Error { message } => Err(message),
                            other => Err(format!("unexpected desktop response: {other:?}")),
                        }
                    });
                let _ = tx.send(result);
            });
            let status = status.clone();
            let output = output.clone();
            let log_buffer = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok((first, second))) => {
                    output.set_text(&format!("{first:#?}\n{second:#?}"));
                    status.set_text("Active-window and workspace metadata shared for 120 seconds.");
                    append_log(&log_buffer, "[context] desktop metadata published");
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    status.set_text(&format!("Context publication failed: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
            });
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let state = state.clone();
        publish_conversation.connect_clicked(move |_| {
            let payload = {
                let state = state.borrow();
                state
                    .conversations
                    .get(state.app_state.active_conversation)
                    .and_then(|conversation| serde_json::to_value(conversation).ok())
                    .unwrap_or(serde_json::Value::Null)
            };
            dispatch_context_request(
                AiIpcRequest::PublishContext {
                    kind: focaldesk_ai::ContextKind::Conversation,
                    provenance: "AI Console active conversation".into(),
                    sensitivity: focaldesk_ai::ContextSensitivity::Restricted,
                    payload,
                    ttl_seconds: 120,
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        grant.connect_clicked(move |_| {
            let parsed = kinds
                .text()
                .split(',')
                .map(str::trim)
                .filter(|kind| !kind.is_empty())
                .map(parse_context_kind)
                .collect::<Result<Vec<_>, _>>();
            let ttl_seconds = ttl.text().parse::<u64>();
            match (parsed, ttl_seconds) {
                (Ok(kinds), Ok(ttl_seconds)) => dispatch_context_request(
                    AiIpcRequest::GrantContext {
                        agent_id: agent_id.text().trim().to_string(),
                        kinds,
                        ttl_seconds,
                    },
                    status.clone(),
                    output.clone(),
                    log_buffer.clone(),
                ),
                _ => status.set_text("Enter valid context kinds and an integer TTL."),
            }
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        revoke.connect_clicked(move |_| {
            dispatch_context_request(
                AiIpcRequest::RevokeContextGrant {
                    grant_id: revoke_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        clear.connect_clicked(move |_| {
            dispatch_context_request(
                AiIpcRequest::ClearContext,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        dismiss_suggestion.connect_clicked(move |_| {
            dispatch_context_request(
                AiIpcRequest::DismissSuggestion {
                    suggestion_id: suggestion_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    page
}

fn parse_context_kind(value: &str) -> Result<focaldesk_ai::ContextKind, String> {
    match value {
        "active_window" => Ok(focaldesk_ai::ContextKind::ActiveWindow),
        "workspace" => Ok(focaldesk_ai::ContextKind::Workspace),
        "notifications" => Ok(focaldesk_ai::ContextKind::Notifications),
        "calendar" => Ok(focaldesk_ai::ContextKind::Calendar),
        "files" => Ok(focaldesk_ai::ContextKind::Files),
        "conversation" => Ok(focaldesk_ai::ContextKind::Conversation),
        _ => Err(format!("unknown context kind: {value}")),
    }
}

fn build_event_fabric_page(log_buffer: TextBuffer) -> Box {
    let page = section_shell(
        "Event Fabric and Consent",
        "Control which typed event sources may disclose which fields. Sources are disabled by default, and only the redacted envelope is retained.",
    );
    page.append(&info_card(&[
        "Desktop and workflow hooks use existing typed service boundaries; calendar, notification, and service producers use the same AI IPC intake.".into(),
        "Only explicitly allowlisted scalar fields enter the bounded in-memory journal.".into(),
        "Simulation and replay evaluate Attention rules without retaining a new event or suggestion.".into(),
        "Emergency disconnect stops all intake immediately without changing saved source policies.".into(),
    ]));

    let status = Label::new(Some("All event sources begin disabled."));
    status.set_xalign(0.0);
    status.set_wrap(true);
    page.append(&status);
    let output = TextBuffer::new(None);
    let view = TextView::with_buffer(&output);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&view)
            .height_request(260)
            .hexpand(true)
            .build(),
    );

    let global_controls = Box::new(Orientation::Horizontal, 8);
    let refresh = Button::with_label("Refresh inspector");
    let disconnect = Button::with_label("Emergency disconnect");
    disconnect.add_css_class("destructive-action");
    let reconnect = Button::with_label("Reconnect fabric");
    let clear = Button::with_label("Clear retained events");
    global_controls.append(&refresh);
    global_controls.append(&disconnect);
    global_controls.append(&reconnect);
    global_controls.append(&clear);
    page.append(&global_controls);

    let connector_id = studio_entry(&page, "Connector ID", "service-health");
    let network_allowed =
        CheckButton::with_label("Allow only the manifest-declared network domains");
    page.append(&network_allowed);
    let connector_controls = Box::new(Orientation::Horizontal, 8);
    let list_connectors = Button::with_label("List connectors");
    let enable_connector = Button::with_label("Enable connector");
    let disable_connector = Button::with_label("Disable connector");
    let rollback_connector = Button::with_label("Rollback connector");
    connector_controls.append(&list_connectors);
    connector_controls.append(&enable_connector);
    connector_controls.append(&disable_connector);
    connector_controls.append(&rollback_connector);
    page.append(&connector_controls);
    let host_controls = Box::new(Orientation::Horizontal, 8);
    let host_status = Button::with_label("Host status");
    let host_poll = Button::with_label("Poll connector now");
    let host_pause = Button::with_label("Pause host");
    let host_resume = Button::with_label("Resume host");
    let clear_quarantine = Button::with_label("Clear quarantine");
    host_controls.append(&host_status);
    host_controls.append(&host_poll);
    host_controls.append(&host_pause);
    host_controls.append(&host_resume);
    host_controls.append(&clear_quarantine);
    page.append(&host_controls);

    let connector_manifest = studio_entry(
        &page,
        "Connector manifest JSON",
        r#"{"manifest_version":1,"id":"example-calendar","name":"Example Calendar","description":"Example SDK connector","version":"1.0.0","event_sources":["calendar"],"event_fields":{"calendar":["event","title","start_time"]},"network_domains":[],"signing_key_handle":"connectors/example-calendar/signing-key","built_in":false}"#,
    );
    let overwrite_connector =
        CheckButton::with_label("Update existing connector and preserve rollback manifest");
    page.append(&overwrite_connector);
    let install_connector = Button::with_label("Install validated connector manifest");
    page.append(&install_connector);

    let source = studio_entry(&page, "Source", "service_health");
    let allowed_fields = studio_entry(
        &page,
        "Disclosed fields",
        "event,service,state,message,failure_count",
    );
    let retention = studio_entry(&page, "Retention seconds", "3600");
    let forward = CheckButton::with_label("Forward redacted summary to Attention");
    forward.set_active(true);
    page.append(&forward);
    let policy_controls = Box::new(Orientation::Horizontal, 8);
    let enable = Button::with_label("Enable source with this policy");
    enable.add_css_class("suggested-action");
    let disable = Button::with_label("Disable source");
    policy_controls.append(&enable);
    policy_controls.append(&disable);
    page.append(&policy_controls);

    let producer = studio_entry(&page, "Producer", "Event Fabric simulator");
    let payload = studio_entry(
        &page,
        "Typed JSON payload",
        r#"{"event":"repeated failure","service":"renderer","state":"failed","failure_count":3}"#,
    );
    let event_controls = Box::new(Orientation::Horizontal, 8);
    let simulate = Button::with_label("Simulate without retention");
    event_controls.append(&simulate);
    page.append(&event_controls);

    let event_id = studio_entry(&page, "Retained event ID", "");
    let replay = Button::with_label("Replay as simulation");
    page.append(&replay);

    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        list_connectors.connect_clicked(move |_| {
            dispatch_event_fabric_request(
                AiIpcRequest::ListConnectors,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        host_status.connect_clicked(move |_| {
            dispatch_connector_host_request(
                ConnectorHostIpcRequest::Status,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let connector_id = connector_id.clone();
        host_poll.connect_clicked(move |_| {
            let connector_id = connector_id.text().trim().to_string();
            dispatch_connector_host_request(
                ConnectorHostIpcRequest::PollNow {
                    connector_id: (!connector_id.is_empty()).then_some(connector_id),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    for (button, paused) in [(host_pause, true), (host_resume, false)] {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        button.connect_clicked(move |_| {
            dispatch_connector_host_request(
                ConnectorHostIpcRequest::SetPaused { paused },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let connector_id = connector_id.clone();
        clear_quarantine.connect_clicked(move |_| {
            dispatch_connector_host_request(
                ConnectorHostIpcRequest::ClearQuarantine {
                    connector_id: connector_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    for (button, enabled) in [(enable_connector, true), (disable_connector, false)] {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let connector_id = connector_id.clone();
        let network_allowed = network_allowed.clone();
        button.connect_clicked(move |_| {
            dispatch_event_fabric_request(
                AiIpcRequest::SetConnectorEnabled {
                    connector_id: connector_id.text().trim().to_string(),
                    enabled,
                    network_allowed: enabled && network_allowed.is_active(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let connector_id = connector_id.clone();
        rollback_connector.connect_clicked(move |_| {
            dispatch_event_fabric_request(
                AiIpcRequest::RollbackConnector {
                    connector_id: connector_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        install_connector.connect_clicked(move |_| {
            let Ok(manifest) =
                serde_json::from_str::<focaldesk_ai::ConnectorManifest>(&connector_manifest.text())
            else {
                status.set_text("Connector manifest must be valid JSON.");
                return;
            };
            dispatch_event_fabric_request(
                AiIpcRequest::InstallConnector {
                    manifest,
                    overwrite: overwrite_connector.is_active(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        refresh.connect_clicked(move |_| {
            dispatch_event_fabric_request(
                AiIpcRequest::GetEventFabricState,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    for (button, connected) in [(disconnect, false), (reconnect, true)] {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        button.connect_clicked(move |_| {
            dispatch_event_fabric_request(
                AiIpcRequest::SetEventFabricConnected { connected },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        clear.connect_clicked(move |_| {
            dispatch_event_fabric_request(
                AiIpcRequest::ClearEventJournal,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    for (button, enabled) in [(enable, true), (disable, false)] {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let source = source.clone();
        let allowed_fields = allowed_fields.clone();
        let retention = retention.clone();
        let forward = forward.clone();
        button.connect_clicked(move |_| {
            let Ok(source) = parse_event_source(source.text().trim()) else {
                status
                    .set_text("Use desktop, calendar, notification, service_health, or workflow.");
                return;
            };
            let Ok(retention_seconds) = retention.text().parse::<u64>() else {
                status.set_text("Retention must be an integer from 60 through 86400.");
                return;
            };
            let fields = if enabled {
                allowed_fields
                    .text()
                    .split(',')
                    .map(str::trim)
                    .filter(|field| !field.is_empty())
                    .map(str::to_string)
                    .collect()
            } else {
                Vec::new()
            };
            dispatch_event_fabric_request(
                AiIpcRequest::ConfigureEventSource {
                    policy: focaldesk_ai::EventSourcePolicy {
                        source,
                        enabled,
                        allowed_fields: fields,
                        retention_seconds,
                        forward_to_attention: forward.is_active(),
                    },
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let source = source.clone();
        let producer = producer.clone();
        let payload = payload.clone();
        simulate.connect_clicked(move |_| {
            let Ok(source) = parse_event_source(source.text().trim()) else {
                status.set_text("Unknown event source.");
                return;
            };
            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload.text()) else {
                status.set_text("Payload must be valid JSON.");
                return;
            };
            dispatch_event_fabric_request(
                AiIpcRequest::SimulateEvent {
                    source,
                    producer: producer.text().trim().to_string(),
                    payload,
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        replay.connect_clicked(move |_| {
            dispatch_event_fabric_request(
                AiIpcRequest::ReplayEventSimulation {
                    event_id: event_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    page
}

fn parse_event_source(value: &str) -> Result<focaldesk_ai::EventSource, String> {
    match value {
        "desktop" => Ok(focaldesk_ai::EventSource::Desktop),
        "calendar" => Ok(focaldesk_ai::EventSource::Calendar),
        "notification" => Ok(focaldesk_ai::EventSource::Notification),
        "service_health" => Ok(focaldesk_ai::EventSource::ServiceHealth),
        "workflow" => Ok(focaldesk_ai::EventSource::Workflow),
        _ => Err(format!("unknown event source: {value}")),
    }
}

fn dispatch_event_fabric_request(
    request: AiIpcRequest,
    status: Label,
    output: TextBuffer,
    log_buffer: TextBuffer,
) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_ai_request(&request).map_err(|error| error.to_string()));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            status.set_text(&format!("Event Fabric request failed: {message}"));
            append_log(&log_buffer, &format!("[event-fabric] {message}"));
            ControlFlow::Break
        }
        Ok(Ok(response)) => {
            output.set_text(&format!("{response:#?}"));
            status.set_text(match response {
                AiIpcResponse::EventFabricState { .. } => "Event Fabric inspector refreshed.",
                AiIpcResponse::EventSourceConfigured { .. } => "Source policy updated.",
                AiIpcResponse::EventFabricConnection { connected: false } => {
                    "Emergency disconnect enabled. New events are rejected."
                }
                AiIpcResponse::EventFabricConnection { connected: true } => {
                    "Event Fabric reconnected; individual source policies still apply."
                }
                AiIpcResponse::EventDelivered { delivery } if delivery.simulated => {
                    "Simulation complete; no event or suggestion was retained."
                }
                AiIpcResponse::EventDelivered { .. } => {
                    "Redacted event retained and eligible Attention rules evaluated."
                }
                AiIpcResponse::EventJournalCleared { .. } => "Retained event journal cleared.",
                AiIpcResponse::Connectors { .. } => "Connector registry refreshed.",
                AiIpcResponse::ConnectorChanged { .. } => "Connector control state updated.",
                _ => "Event Fabric request completed.",
            });
            append_log(&log_buffer, "[event-fabric] request completed");
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Event Fabric request channel disconnected.");
            ControlFlow::Break
        }
    });
}

fn dispatch_connector_host_request(
    request: ConnectorHostIpcRequest,
    status: Label,
    output: TextBuffer,
    log_buffer: TextBuffer,
) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_connector_host_request(&request));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(response)) => {
            output.set_text(&format!("{response:#?}"));
            if response.status == "error" {
                status.set_text(
                    response
                        .message
                        .as_deref()
                        .unwrap_or("Connector host request failed."),
                );
            } else if response.paused {
                status.set_text("Managed connector host is paused.");
            } else {
                status.set_text("Managed connector host state refreshed.");
            }
            append_log(&log_buffer, "[connector-host] request completed");
            ControlFlow::Break
        }
        Ok(Err(error)) => {
            status.set_text(&format!("Connector host unavailable: {error}"));
            append_log(&log_buffer, &format!("[connector-host] {error}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
    });
}

fn build_attention_page(log_buffer: TextBuffer) -> Box {
    let page = section_shell(
        "Attention and Routines",
        "Evaluate typed events against bounded routines. Matching events create suggestions only; promotion is always an explicit second step.",
    );
    page.append(&info_card(&[
        "Built-in routines cover morning briefing, meeting preparation, and repeated service failure.".into(),
        "Quiet hours, cooldowns, duplicate suppression, and hourly limits reduce interruption.".into(),
        "Simulation never changes state. The emergency pause suppresses every routine.".into(),
        "Promoted work still uses normal agent capabilities, permissions, and native confirmations.".into(),
    ]));

    let status = Label::new(Some("Refresh to inspect routines and suggestions."));
    status.set_xalign(0.0);
    status.set_wrap(true);
    page.append(&status);
    let output = TextBuffer::new(None);
    let view = TextView::with_buffer(&output);
    view.set_editable(false);
    view.set_cursor_visible(false);
    view.set_monospace(true);
    view.set_wrap_mode(gtk4::WrapMode::WordChar);
    page.append(
        &ScrolledWindow::builder()
            .child(&view)
            .height_request(260)
            .hexpand(true)
            .build(),
    );

    let state_controls = Box::new(Orientation::Horizontal, 8);
    let refresh = Button::with_label("Refresh routines");
    let pause = Button::with_label("Emergency pause");
    pause.add_css_class("destructive-action");
    let resume = Button::with_label("Resume routines");
    state_controls.append(&refresh);
    state_controls.append(&pause);
    state_controls.append(&resume);
    page.append(&state_controls);

    let event_kind = studio_entry(&page, "Event kind", "service");
    let event_value = studio_entry(
        &page,
        "Typed event value",
        "renderer repeated failure detected",
    );
    let event_source = studio_entry(&page, "Event source", "Attention simulator");
    let event_controls = Box::new(Orientation::Horizontal, 8);
    let simulate = Button::with_label("Simulate");
    let dispatch = Button::with_label("Publish matching suggestion");
    dispatch.add_css_class("suggested-action");
    event_controls.append(&simulate);
    event_controls.append(&dispatch);
    page.append(&event_controls);

    let suggestion_id = studio_entry(&page, "Suggestion ID", "");
    let suggestion_controls = Box::new(Orientation::Horizontal, 8);
    let promote = Button::with_label("Promote to agent/workflow");
    promote.add_css_class("suggested-action");
    let dismiss = Button::with_label("Dismiss suggestion");
    suggestion_controls.append(&promote);
    suggestion_controls.append(&dismiss);
    page.append(&suggestion_controls);

    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        refresh.connect_clicked(move |_| {
            dispatch_attention_request(
                AiIpcRequest::GetRoutineState,
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    for (button, suspended) in [(pause, true), (resume, false)] {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        button.connect_clicked(move |_| {
            dispatch_attention_request(
                AiIpcRequest::SetRoutinesSuspended { suspended },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    for (button, simulated) in [(simulate, true), (dispatch, false)] {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let event_kind = event_kind.clone();
        let event_value = event_value.clone();
        let event_source = event_source.clone();
        button.connect_clicked(move |_| {
            let Ok(kind) = parse_routine_event_kind(event_kind.text().trim()) else {
                status.set_text(
                    "Use desktop, calendar, notification, service, workflow, or context.",
                );
                return;
            };
            let event = focaldesk_ai::RoutineEvent {
                kind,
                value: event_value.text().trim().to_string(),
                source: event_source.text().trim().to_string(),
            };
            let request = if simulated {
                AiIpcRequest::SimulateRoutineEvent { event }
            } else {
                AiIpcRequest::DispatchRoutineEvent { event }
            };
            dispatch_attention_request(request, status.clone(), output.clone(), log_buffer.clone());
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        let suggestion_id = suggestion_id.clone();
        promote.connect_clicked(move |_| {
            dispatch_attention_request(
                AiIpcRequest::PromoteRoutineSuggestion {
                    suggestion_id: suggestion_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    {
        let status = status.clone();
        let output = output.clone();
        let log_buffer = log_buffer.clone();
        dismiss.connect_clicked(move |_| {
            dispatch_attention_request(
                AiIpcRequest::DismissRoutineSuggestion {
                    suggestion_id: suggestion_id.text().trim().to_string(),
                },
                status.clone(),
                output.clone(),
                log_buffer.clone(),
            );
        });
    }
    page
}

fn parse_routine_event_kind(value: &str) -> Result<focaldesk_ai::RoutineEventKind, String> {
    match value {
        "desktop" => Ok(focaldesk_ai::RoutineEventKind::Desktop),
        "calendar" => Ok(focaldesk_ai::RoutineEventKind::Calendar),
        "notification" => Ok(focaldesk_ai::RoutineEventKind::Notification),
        "service" => Ok(focaldesk_ai::RoutineEventKind::Service),
        "workflow" => Ok(focaldesk_ai::RoutineEventKind::Workflow),
        "context" => Ok(focaldesk_ai::RoutineEventKind::Context),
        _ => Err(format!("unknown routine event kind: {value}")),
    }
}

fn dispatch_attention_request(
    request: AiIpcRequest,
    status: Label,
    output: TextBuffer,
    log_buffer: TextBuffer,
) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_ai_request(&request).map_err(|error| error.to_string()));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            status.set_text(&format!("Attention request failed: {message}"));
            append_log(&log_buffer, &format!("[attention] {message}"));
            ControlFlow::Break
        }
        Ok(Ok(response)) => {
            output.set_text(&format!("{response:#?}"));
            status.set_text(match response {
                AiIpcResponse::RoutineState { .. } => "Routine inspector refreshed.",
                AiIpcResponse::RoutineEvaluated { .. } => "Routine evaluation complete.",
                AiIpcResponse::RoutinesSuspended { suspended: true } => {
                    "Emergency pause enabled. All routine events are suppressed."
                }
                AiIpcResponse::RoutinesSuspended { suspended: false } => "Routines resumed.",
                AiIpcResponse::RoutineSuggestionPromoted { .. } => {
                    "Suggestion explicitly promoted; the run is visible in Agent Studio."
                }
                AiIpcResponse::RoutineSuggestionDismissed { .. } => "Suggestion dismissed.",
                _ => "Attention request completed.",
            });
            append_log(&log_buffer, "[attention] request completed");
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Attention request channel disconnected.");
            ControlFlow::Break
        }
    });
}

fn dispatch_context_request(
    request: AiIpcRequest,
    status: Label,
    output: TextBuffer,
    log_buffer: TextBuffer,
) {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_ai_request(&request).map_err(|error| error.to_string()));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::ContextState {
            envelopes,
            grants,
            suggestions,
        })) => {
            output.set_text(
                &serde_json::to_string_pretty(&(envelopes, grants, suggestions))
                    .unwrap_or_default(),
            );
            status.set_text("Context inspector refreshed.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::ContextPublished { envelope })) => {
            output.set_text(&serde_json::to_string_pretty(&envelope).unwrap_or_default());
            status.set_text("Expiring context envelope published.");
            append_log(&log_buffer, "[context] envelope published");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::ContextGranted { grant })) => {
            output.set_text(&serde_json::to_string_pretty(&grant).unwrap_or_default());
            status.set_text("Expiring agent context grant created.");
            append_log(&log_buffer, "[context] grant created");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::ContextGrantRevocation { revoked, .. })) => {
            status.set_text(if revoked {
                "Context grant revoked."
            } else {
                "Grant was already inactive or unknown."
            });
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::ContextCleared { cleared })) => {
            output.set_text("");
            status.set_text(&format!("Cleared {cleared} context envelopes."));
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::SuggestionPublished { suggestion })) => {
            output.set_text(&serde_json::to_string_pretty(&suggestion).unwrap_or_default());
            status.set_text("Suggestion added to the inbox; no action was executed.");
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::SuggestionDismissed { dismissed, .. })) => {
            status.set_text(if dismissed {
                "Suggestion dismissed."
            } else {
                "Suggestion was already inactive or unknown."
            });
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            status.set_text(&format!("Context request failed: {message}"));
            ControlFlow::Break
        }
        Ok(Ok(other)) => {
            status.set_text(&format!("Unexpected context response: {other:?}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
    });
}

#[allow(clippy::too_many_arguments)]
fn build_ambient_voice_page(
    state: Rc<RefCell<PersistedState>>,
    chat_view: Box,
    conversation_detail: Box,
    entry: Entry,
    send_button: Button,
    stream_controls: ChatStreamControls,
    log_buffer: TextBuffer,
) -> Box {
    let page = section_shell(
        "Ambient Voice",
        "Offline, wake-gated voice commands. Listening is opt-in for this session and every command uses the existing agent, workflow, and confirmation boundaries.",
    );
    page.append(&info_card(&[
        "Raw audio stays in a bounded RAM-only rolling buffer and is discarded on stop or kill."
            .into(),
        "The transcript is off by default and is never written to disk from this page.".into(),
        "Say “hello focaldesk”, then a request. Voice cannot approve a proposed mutation.".into(),
    ]));

    let wake_row = Box::new(Orientation::Horizontal, 8);
    let wake_label = Label::new(Some("Wake phrase"));
    wake_label.set_xalign(0.0);
    wake_label.set_width_chars(18);
    let wake_phrase = Entry::new();
    wake_phrase.set_hexpand(true);
    wake_phrase.set_text("hello focaldesk");
    wake_phrase.set_max_length(80);
    wake_row.append(&wake_label);
    wake_row.append(&wake_phrase);
    page.append(&wake_row);

    let block_row = Box::new(Orientation::Horizontal, 8);
    let block_label = Label::new(Some("Blocked applications"));
    block_label.set_xalign(0.0);
    block_label.set_width_chars(18);
    let blocked_applications = Entry::new();
    blocked_applications.set_hexpand(true);
    blocked_applications.set_placeholder_text(Some("comma-separated application IDs"));
    block_row.append(&block_label);
    block_row.append(&blocked_applications);
    page.append(&block_row);

    let transcript_row = Box::new(Orientation::Horizontal, 8);
    let transcript_label = Label::new(Some("Keep in-memory transcript"));
    transcript_label.set_xalign(0.0);
    transcript_label.set_hexpand(true);
    let transcript_enabled = Switch::new();
    transcript_enabled.set_active(false);
    transcript_row.append(&transcript_label);
    transcript_row.append(&transcript_enabled);
    page.append(&transcript_row);

    let speech_row = Box::new(Orientation::Horizontal, 8);
    let speech_label = Label::new(Some("Speak responses locally"));
    speech_label.set_xalign(0.0);
    speech_label.set_hexpand(true);
    let speech_enabled = Switch::new();
    speech_enabled.set_active(true);
    speech_row.append(&speech_label);
    speech_row.append(&speech_enabled);
    page.append(&speech_row);

    let initial_microphone = send_microphone_request(&MicrophoneIpcRequest::Status).ok();
    let initially_killed = initial_microphone
        .as_ref()
        .is_some_and(|response| response.killed);
    let initial_status = if initially_killed {
        "MICROPHONE KILLED · capture is disabled".to_string()
    } else if let Some(owner) = initial_microphone
        .as_ref()
        .and_then(|response| response.owner.as_deref())
    {
        format!("Mic in use · leased to {owner}")
    } else {
        "Mic off · press Start listening to opt in".to_string()
    };
    let status = Label::new(Some(&initial_status));
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("mode-banner-body");
    page.append(&status);

    let controls = Box::new(Orientation::Horizontal, 8);
    let listen = Button::with_label("Start listening");
    listen.add_css_class("suggested-action");
    listen.set_sensitive(
        !initially_killed
            && initial_microphone
                .as_ref()
                .is_none_or(|response| response.owner.is_none()),
    );
    let kill = Button::with_label("Kill microphone now");
    kill.add_css_class("destructive-action");
    let enable = Button::with_label("Re-enable microphone");
    enable.set_sensitive(initially_killed);
    controls.append(&listen);
    controls.append(&kill);
    controls.append(&enable);
    page.append(&controls);

    let transcript_buffer = TextBuffer::new(None);
    transcript_buffer.set_text("Transcript retention is off.");
    {
        let transcript_buffer = transcript_buffer.clone();
        transcript_enabled.connect_active_notify(move |control| {
            transcript_buffer.set_text(if control.is_active() {
                ""
            } else {
                "Transcript retention is off."
            });
        });
    }
    let transcript_view = TextView::with_buffer(&transcript_buffer);
    transcript_view.set_editable(false);
    transcript_view.set_cursor_visible(false);
    transcript_view.set_monospace(true);
    transcript_view.set_wrap_mode(gtk4::WrapMode::WordChar);
    let transcript_scroll = ScrolledWindow::builder()
        .child(&transcript_view)
        .height_request(140)
        .hexpand(true)
        .build();
    page.append(&transcript_scroll);
    let clear_transcript = Button::with_label("Clear transcript");
    clear_transcript.set_halign(gtk4::Align::Start);
    page.append(&clear_transcript);

    let current_session: Rc<RefCell<Option<AmbientMicClient>>> = Rc::new(RefCell::new(None));
    {
        let transcript_buffer = transcript_buffer.clone();
        let current_session = current_session.clone();
        let log_buffer = log_buffer.clone();
        clear_transcript.connect_clicked(move |_| {
            transcript_buffer.set_text("");
            if let Some(session) = current_session.borrow().as_ref()
                && let Err(error) = send_microphone_request(&MicrophoneIpcRequest::ClearEvents {
                    lease_id: session.lease_id.clone(),
                })
            {
                append_log(
                    &log_buffer,
                    &format!("[ambient-voice] could not clear daemon context: {error}"),
                );
            }
        });
    }
    {
        let current_session = current_session.clone();
        let status = status.clone();
        let listen = listen.clone();
        let enable = enable.clone();
        let log_buffer = log_buffer.clone();
        kill.connect_clicked(move |_| {
            if let Some(session) = current_session.borrow().as_ref() {
                session.stop_poll.store(true, Ordering::SeqCst);
            }
            if let Err(error) = send_microphone_request(&MicrophoneIpcRequest::Kill) {
                status.set_text(&format!("Microphone kill failed: {error}"));
                append_log(
                    &log_buffer,
                    &format!("[ambient-voice] kill failed: {error}"),
                );
                return;
            }
            listen.set_sensitive(false);
            listen.set_label("Start listening");
            enable.set_sensitive(true);
            status.set_text("MICROPHONE KILLED · capture is disabled");
            append_log(
                &log_buffer,
                "[ambient-voice] microphone kill switch activated",
            );
        });
    }
    {
        let status = status.clone();
        let listen = listen.clone();
        let enable = enable.clone();
        let log_buffer = log_buffer.clone();
        enable.clone().connect_clicked(move |_| {
            if let Err(error) = send_microphone_request(&MicrophoneIpcRequest::Enable) {
                status.set_text(&format!("Could not re-enable microphone: {error}"));
                append_log(
                    &log_buffer,
                    &format!("[ambient-voice] re-enable failed: {error}"),
                );
                return;
            }
            listen.set_sensitive(true);
            enable.set_sensitive(false);
            status.set_text("Mic off · press Start listening to opt in");
            append_log(
                &log_buffer,
                "[ambient-voice] microphone gate re-enabled; capture remains off",
            );
        });
    }

    {
        let current_session = current_session.clone();
        let status = status.clone();
        let wake_phrase = wake_phrase.clone();
        let blocked_applications = blocked_applications.clone();
        let transcript_enabled = transcript_enabled.clone();
        let speech_enabled = speech_enabled.clone();
        let transcript_buffer = transcript_buffer.clone();
        let state = state.clone();
        let chat_view = chat_view.clone();
        let conversation_detail = conversation_detail.clone();
        let entry = entry.clone();
        let send_button = send_button.clone();
        let stream_controls = stream_controls.clone();
        let log_buffer = log_buffer.clone();
        listen.connect_clicked(move |button| {
            if let Some(session) = current_session.borrow().as_ref() {
                session.stop_poll.store(true, Ordering::SeqCst);
                let lease_id = session.lease_id.clone();
                thread::spawn(move || {
                    let _ = send_microphone_request(&MicrophoneIpcRequest::Stop {
                        lease_id: Some(lease_id),
                    });
                });
                button.set_label("Start listening");
                status.set_text("Stopping · discarding buffered audio…");
                return;
            }
            let request = MicrophoneIpcRequest::StartAmbient {
                requester: "focaldesk-ai-console".into(),
                wake_phrase: wake_phrase.text().trim().to_string(),
                blocked_applications: blocked_applications
                    .text()
                    .split(',')
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect(),
            };
            let response = match send_microphone_request(&request) {
                Ok(response) if response.status != "error" => response,
                Ok(response) => {
                    let error = response.message.unwrap_or_else(|| "request rejected".into());
                    status.set_text(&format!("Could not start listening: {error}"));
                    append_log(&log_buffer, &format!("[ambient-voice] {error}"));
                    return;
                }
                Err(error) => {
                    status.set_text(&format!("Could not start listening: {error}"));
                    append_log(&log_buffer, &format!("[ambient-voice] {error}"));
                    return;
                }
            };
            let Some(lease_id) = response.lease_id else {
                status.set_text("Voice daemon did not return a microphone lease");
                return;
            };
            let stop_poll = Arc::new(AtomicBool::new(false));
            *current_session.borrow_mut() = Some(AmbientMicClient {
                lease_id: lease_id.clone(),
                stop_poll: stop_poll.clone(),
            });
            button.set_label("Stop listening");
            status.set_text("Opening microphone…");
            append_log(&log_buffer, "[ambient-voice] opt-in listening requested");

            let current_session = current_session.clone();
            let status = status.clone();
            let button = button.clone();
            let transcript_enabled = transcript_enabled.clone();
            let speech_enabled = speech_enabled.clone();
            let transcript_buffer = transcript_buffer.clone();
            let state = state.clone();
            let chat_view = chat_view.clone();
            let conversation_detail = conversation_detail.clone();
            let entry = entry.clone();
            let send_button = send_button.clone();
            let stream_controls = stream_controls.clone();
            let log_buffer = log_buffer.clone();
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let mut after_sequence = 0;
                while !stop_poll.load(Ordering::SeqCst) {
                    match send_microphone_request(&MicrophoneIpcRequest::Poll {
                        lease_id: lease_id.clone(),
                        after_sequence,
                    }) {
                        Ok(response) => {
                            after_sequence = response.latest_sequence.max(after_sequence);
                            let stopped = response.status == "idle" || response.killed;
                            if tx.send(Ok(response)).is_err() || stopped {
                                break;
                            }
                        }
                        Err(error) => {
                            let _ = tx.send(Err(error));
                            break;
                        }
                    }
                    thread::sleep(Duration::from_millis(80));
                }
            });
            glib::timeout_add_local(Duration::from_millis(60), move || {
                loop {
                    match rx.try_recv() {
                        Ok(Ok(response)) => {
                            for record in response.events {
                                match record.event {
                                    MicrophoneEvent::Ready => {
                                        status.set_text("LISTENING · say the wake phrase")
                                    }
                                    MicrophoneEvent::VoiceActivity(true) => {
                                        status.set_text("HEARING SPEECH · local recognition only");
                                        if let Some(request_id) = stream_controls.active_request_id.borrow().clone() {
                                            append_log(&log_buffer, "[ambient-voice] barge-in requested chat cancellation");
                                            thread::spawn(move || { let _ = cancel_ai_stream(&request_id); });
                                        }
                                    }
                                    MicrophoneEvent::VoiceActivity(false) => status.set_text("LISTENING · processing speech locally"),
                                    MicrophoneEvent::WakeDetected => status.set_text("WAKE DETECTED · waiting for command"),
                                    MicrophoneEvent::Command(command) => {
                                        status.set_text(&format!("ROUTING · {command}"));
                                        if transcript_enabled.is_active() {
                                            let mut end = transcript_buffer.end_iter();
                                            transcript_buffer.insert(&mut end, &format!("{command}\n"));
                                        }
                                        route_ambient_command(command, state.clone(), chat_view.clone(), conversation_detail.clone(), entry.clone(), send_button.clone(), stream_controls.clone(), status.clone(), log_buffer.clone(), speech_enabled.is_active());
                                    }
                                    MicrophoneEvent::Error(error) => {
                                        status.set_text(&format!("Voice runtime stopped: {error}"));
                                        append_log(&log_buffer, &format!("[ambient-voice] {error}"));
                                    }
                                    MicrophoneEvent::Partial(_) | MicrophoneEvent::Final(_) | MicrophoneEvent::Stopped => {}
                                }
                            }
                            if response.status == "idle" || response.killed {
                                button.set_label("Start listening");
                                *current_session.borrow_mut() = None;
                                status.set_text(if response.killed { "MICROPHONE KILLED · capture is disabled" } else { "Mic off · buffered audio discarded" });
                                return ControlFlow::Break;
                            }
                        }
                        Ok(Err(error)) => {
                            button.set_label("Start listening");
                            *current_session.borrow_mut() = None;
                            status.set_text(&format!("Voice runtime stopped: {error}"));
                            append_log(&log_buffer, &format!("[ambient-voice] {error}"));
                            return ControlFlow::Break;
                        }
                        Err(mpsc::TryRecvError::Empty) => return ControlFlow::Continue,
                        Err(mpsc::TryRecvError::Disconnected) => {
                            button.set_label("Start listening");
                            *current_session.borrow_mut() = None;
                            return ControlFlow::Break;
                        }
                    }
                }
            });
        });
    }

    page
}

#[allow(clippy::too_many_arguments)]
fn route_ambient_command(
    command: String,
    state: Rc<RefCell<PersistedState>>,
    chat_view: Box,
    conversation_detail: Box,
    entry: Entry,
    send_button: Button,
    stream_controls: ChatStreamControls,
    status: Label,
    log_buffer: TextBuffer,
    speak_response: bool,
) {
    let normalized = command.trim();
    let request = if let Some(workflow_id) = normalized.strip_prefix("run workflow ") {
        Some(AiIpcRequest::StartWorkflow {
            workflow_id: workflow_id.trim().to_string(),
        })
    } else if let Some(agent_command) = normalized.strip_prefix("ask agent ") {
        agent_command
            .split_once(' ')
            .map(|(agent_id, objective)| AiIpcRequest::StartAgent {
                request: AgentRequest {
                    objective: objective.trim().trim_start_matches("to ").to_string(),
                    agent_id: Some(agent_id.to_string()),
                    provider: None,
                    model: None,
                },
            })
    } else {
        match focaldesk_ai::route_intent(normalized).destination {
            focaldesk_ai::IntentDestination::Workflow { workflow_id } => {
                Some(AiIpcRequest::StartWorkflow { workflow_id })
            }
            focaldesk_ai::IntentDestination::Agent { agent_id } => Some(AiIpcRequest::StartAgent {
                request: AgentRequest {
                    objective: normalized.to_string(),
                    agent_id: Some(agent_id),
                    provider: None,
                    model: None,
                },
            }),
            focaldesk_ai::IntentDestination::Chat
            | focaldesk_ai::IntentDestination::SuggestionInbox => None,
        }
    };

    let Some(request) = request else {
        dispatch_chat_request_async(
            state,
            chat_view,
            conversation_detail,
            entry,
            send_button,
            stream_controls,
            log_buffer,
            normalized.to_string(),
            if speak_response {
                "ambient voice+speech"
            } else {
                "ambient voice"
            },
            None,
        );
        return;
    };

    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_ai_request(&request).map_err(|error| error.to_string()));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::WorkflowStarted { run_id })) => {
            status.set_text(&format!("Workflow started · {run_id}"));
            append_log(
                &log_buffer,
                &format!("[ambient-voice] workflow run {run_id} started"),
            );
            if speak_response {
                speak_local_async("Workflow started".into(), log_buffer.clone());
            }
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::AgentStarted { run_id })) => {
            status.set_text(&format!("Agent started · {run_id}"));
            append_log(
                &log_buffer,
                &format!("[ambient-voice] agent run {run_id} started"),
            );
            if speak_response {
                speak_local_async("Agent started".into(), log_buffer.clone());
            }
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            status.set_text(&format!("Command failed · {message}"));
            append_log(
                &log_buffer,
                &format!("[ambient-voice] command failed: {message}"),
            );
            ControlFlow::Break
        }
        Ok(Ok(other)) => {
            status.set_text("Command returned an unexpected response");
            append_log(
                &log_buffer,
                &format!("[ambient-voice] unexpected response: {other:?}"),
            );
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Command failed · AI service disconnected");
            ControlFlow::Break
        }
    });
}

#[derive(Clone, Copy)]
struct CodingAgentDefinition {
    id: &'static str,
    label: &'static str,
    command: &'static str,
}

const CODING_AGENTS: &[CodingAgentDefinition] = &[
    CodingAgentDefinition {
        id: "codex",
        label: "Codex",
        command: "codex",
    },
    CodingAgentDefinition {
        id: "claude",
        label: "Claude Code",
        command: "claude",
    },
    CodingAgentDefinition {
        id: "opencode",
        label: "OpenCode",
        command: "opencode",
    },
    CodingAgentDefinition {
        id: "cursor-agent",
        label: "Cursor Agent",
        command: "cursor-agent",
    },
    CodingAgentDefinition {
        id: "copilot",
        label: "GitHub Copilot",
        command: "copilot",
    },
    CodingAgentDefinition {
        id: "gemini",
        label: "Gemini CLI",
        command: "gemini",
    },
    CodingAgentDefinition {
        id: "grok",
        label: "Grok",
        command: "grok",
    },
    CodingAgentDefinition {
        id: "pi",
        label: "Pi",
        command: "pi",
    },
];

fn default_coding_agent() -> String {
    "codex".to_string()
}

fn default_coding_agent_working_directory() -> String {
    dirs::home_dir()
        .map(|home| preferred_coding_agent_workdir(&home))
        .unwrap_or_else(|| PathBuf::from("."))
        .display()
        .to_string()
}

fn preferred_coding_agent_workdir(home: &Path) -> PathBuf {
    let work = home.join("Work");
    if work.is_dir() {
        work
    } else {
        home.to_path_buf()
    }
}

fn build_coding_agents_page(state: Rc<RefCell<PersistedState>>, log_buffer: TextBuffer) -> Box {
    let page = Box::new(Orientation::Vertical, 12);
    page.add_css_class("detail-pane");

    let title = Label::new(Some("Coding Agents"));
    title.set_xalign(0.0);
    title.add_css_class("page-title");
    page.append(&title);

    let description = Label::new(Some(
        "Choose a default coding-agent harness and launch it in a dedicated terminal. FocalDesk uses the installed CLI directly and never copies its authentication tokens.",
    ));
    description.set_xalign(0.0);
    description.set_wrap(true);
    description.add_css_class("mode-banner-body");
    page.append(&description);

    let status = Label::new(None);
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.add_css_class("source-status");
    refresh_coding_agent_status(&status, &state.borrow().app_state.default_coding_agent);
    page.append(&status);

    let directory_label = Label::new(Some("Project directory"));
    directory_label.set_xalign(0.0);
    directory_label.add_css_class("section-title");
    page.append(&directory_label);

    let directory_entry = Entry::builder()
        .text(&state.borrow().app_state.coding_agent_working_directory)
        .placeholder_text("/absolute/path/to/project")
        .hexpand(true)
        .build();
    let choose_directory = Button::with_label("Choose…");
    let directory_row = Box::new(Orientation::Horizontal, 8);
    directory_row.append(&directory_entry);
    directory_row.append(&choose_directory);
    page.append(&directory_row);

    {
        let state = state.clone();
        directory_entry.connect_changed(move |entry| {
            state.borrow_mut().app_state.coding_agent_working_directory = entry.text().to_string();
            persist_state(&state.borrow());
        });
    }
    {
        let directory_entry = directory_entry.clone();
        choose_directory.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Choose a coding project directory")
                .accept_label("Use Directory")
                .modal(true)
                .build();
            let directory_entry = directory_entry.clone();
            dialog.select_folder(
                None::<&gtk4::Window>,
                gtk4::gio::Cancellable::NONE,
                move |selection| {
                    if let Ok(folder) = selection
                        && let Some(path) = folder.path()
                    {
                        directory_entry.set_text(&path.display().to_string());
                    }
                },
            );
        });
    }

    let launch_default = Button::with_label("Launch default agent");
    launch_default.add_css_class("suggested-action");
    {
        let state = state.clone();
        let status = status.clone();
        let log_buffer = log_buffer.clone();
        launch_default.connect_clicked(move |_| {
            let state = state.borrow();
            launch_coding_agent_async(
                &state.app_state.default_coding_agent,
                &state.app_state.coding_agent_working_directory,
                status.clone(),
                log_buffer.clone(),
            );
        });
    }
    page.append(&launch_default);

    let list = Box::new(Orientation::Vertical, 8);
    list.add_css_class("item-card");
    for agent in CODING_AGENTS {
        let row = Box::new(Orientation::Horizontal, 8);
        let installed = executable_available(agent.command);
        let label = Label::new(Some(&format!(
            "{} · {}",
            agent.label,
            if installed {
                "installed"
            } else {
                "not installed"
            }
        )));
        label.set_xalign(0.0);
        label.set_hexpand(true);

        let use_button = Button::with_label("Use as default");
        let agent_id = agent.id.to_string();
        {
            let state = state.clone();
            let status = status.clone();
            use_button.connect_clicked(move |_| {
                let mut state = state.borrow_mut();
                state.app_state.default_coding_agent = agent_id.clone();
                persist_state(&state);
                refresh_coding_agent_status(&status, &agent_id);
            });
        }

        let launch_button = Button::with_label("Launch");
        launch_button.set_sensitive(installed);
        let agent_id = agent.id.to_string();
        {
            let state = state.clone();
            let status = status.clone();
            let log_buffer = log_buffer.clone();
            launch_button.connect_clicked(move |_| {
                let working_directory = state
                    .borrow()
                    .app_state
                    .coding_agent_working_directory
                    .clone();
                launch_coding_agent_async(
                    &agent_id,
                    &working_directory,
                    status.clone(),
                    log_buffer.clone(),
                );
            });
        }

        row.append(&label);
        row.append(&use_button);
        row.append(&launch_button);
        list.append(&row);
    }
    page.append(&list);

    let install_note = Label::new(Some(
        "Install missing harnesses with their official package manager or installer, then reopen AI Console. The FocalDesk skill is installed for supported harness locations by `just install-ai`.",
    ));
    install_note.set_xalign(0.0);
    install_note.set_wrap(true);
    install_note.add_css_class("source-status");
    page.append(&install_note);

    page
}

fn coding_agent(agent_id: &str) -> Option<CodingAgentDefinition> {
    CODING_AGENTS
        .iter()
        .copied()
        .find(|agent| agent.id == agent_id)
}

fn refresh_coding_agent_status(status: &Label, agent_id: &str) {
    let Some(agent) = coding_agent(agent_id) else {
        status.set_text("The configured default coding agent is unknown.");
        return;
    };
    status.set_text(&format!(
        "Default: {} · {}",
        agent.label,
        if executable_available(agent.command) {
            "ready"
        } else {
            "CLI not found on PATH"
        }
    ));
}

fn executable_available(command: &str) -> bool {
    resolve_executable(command).is_some()
}

fn resolve_executable(command: &str) -> Option<PathBuf> {
    let path = Path::new(command);
    if path.components().count() > 1 {
        return is_executable_file(path).then(|| path.to_path_buf());
    }

    executable_search_dirs(
        std::env::var_os("PATH").as_deref(),
        dirs::home_dir().as_deref(),
    )
    .into_iter()
    .map(|directory| directory.join(command))
    .find(|candidate| is_executable_file(candidate))
}

fn executable_search_dirs(
    inherited_path: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Vec<PathBuf> {
    let mut directories = inherited_path
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    // Graphical sessions often start before interactive shell startup files
    // extend PATH. Search common user-level CLI install roots explicitly so
    // the status shown here agrees with the user's terminal.
    if let Some(home) = home {
        for relative in [
            ".local/bin",
            ".cargo/bin",
            ".npm-global/bin",
            ".bun/bin",
            ".local/share/pnpm",
            ".volta/bin",
        ] {
            let candidate = home.join(relative);
            if !directories.contains(&candidate) {
                directories.push(candidate);
            }
        }
    }

    directories
}

fn is_executable_file(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

fn coding_agent_terminal_args(terminal: &str, agent_executable: &Path) -> Vec<OsString> {
    let terminal_name = Path::new(terminal)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(terminal);
    if terminal_name == "weston-terminal" {
        return vec![OsString::from(format!(
            "--shell={}",
            agent_executable.display()
        ))];
    }

    vec![OsString::from("-e"), agent_executable.as_os_str().into()]
}

fn resolve_coding_agent_workdir(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if configured.is_empty() {
        return None;
    }
    let path = if configured == "~" {
        dirs::home_dir()?
    } else if let Some(relative) = configured.strip_prefix("~/") {
        dirs::home_dir()?.join(relative)
    } else {
        PathBuf::from(configured)
    };
    path.is_dir().then_some(path)
}

fn launch_coding_agent_async(
    agent_id: &str,
    working_directory: &str,
    status: Label,
    log_buffer: TextBuffer,
) {
    let Some(agent) = coding_agent(agent_id) else {
        status.set_text("Cannot launch an unknown coding agent.");
        return;
    };
    let Some(agent_executable) = resolve_executable(agent.command) else {
        status.set_text(&format!(
            "{} is not installed or is not on PATH.",
            agent.label
        ));
        return;
    };
    let Some(workdir) = resolve_coding_agent_workdir(working_directory) else {
        status.set_text("Choose an existing project directory before launching the agent.");
        return;
    };

    status.set_text(&format!(
        "Launching {} in {}…",
        agent.label,
        workdir.display()
    ));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let settings = load_settings();
        let terminal_args = coding_agent_terminal_args(&settings.apps.terminal, &agent_executable);
        let result = Command::new(&settings.apps.terminal)
            .args(terminal_args)
            .current_dir(&workdir)
            .spawn()
            .with_context(|| {
                format!(
                    "failed to launch {} with terminal {}",
                    agent.label, settings.apps.terminal
                )
            })
            .map(|_| (agent.label.to_string(), workdir));
        let _ = tx.send(result);
    });

    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok((label, workdir))) => {
            status.set_text(&format!("Launched {label} in {}.", workdir.display()));
            append_log(
                &log_buffer,
                &format!(
                    "[agent] launched external harness {label} in {}",
                    workdir.display()
                ),
            );
            ControlFlow::Break
        }
        Ok(Err(error)) => {
            status.set_text(&format!("Agent launch failed: {error}"));
            append_log(&log_buffer, &format!("[agent] launch failed: {error}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            status.set_text("Agent launch failed: background task disconnected.");
            append_log(&log_buffer, "[agent] launch task disconnected");
            ControlFlow::Break
        }
    });
}

fn dispatch_desktop_agent(
    page: Rc<DesktopAgentPage>,
    state: Rc<RefCell<PersistedState>>,
    runtime: Rc<RefCell<AiConsoleRuntime>>,
    log_buffer: TextBuffer,
) {
    if !page.run_button.is_sensitive() {
        return;
    }
    let objective = page.objective.text().trim().to_string();
    if objective.is_empty() {
        page.status
            .set_text("Enter an objective before running the agent.");
        return;
    }

    let (provider, model) = {
        let state = state.borrow();
        let runtime = runtime.borrow();
        let provider = if state.app_state.active_provider.is_empty() {
            runtime.default_provider.clone()
        } else {
            Some(state.app_state.active_provider.clone())
        };
        (provider, effective_request_model(&state))
    };

    page.run_button.set_sensitive(false);
    page.approve_button.set_sensitive(false);
    page.deny_button.set_sensitive(false);
    page.pending_plan_id.borrow_mut().take();
    page.status
        .set_text("Waiting for permission and planning the bounded desktop inspection…");
    page.result_buffer.set_text("Planning…");
    append_log(&log_buffer, "[agent] desktop objective submitted");

    let request = AgentRequest {
        objective,
        agent_id: page.agent_profile.selected_id(),
        provider,
        model,
    };
    let (tx, rx) = mpsc::channel::<Result<AiIpcResponse, String>>();
    thread::spawn(move || {
        let _ = tx.send(
            send_ai_request(&AiIpcRequest::StartAgent { request })
                .map_err(|error| error.to_string()),
        );
    });

    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::AgentStarted { run_id })) => {
            *page.active_run_id.borrow_mut() = Some(run_id.clone());
            page.cancel_button.set_sensitive(true);
            page.status.set_text(&format!("Agent run {run_id} started"));
            append_log(&log_buffer, &format!("[agent] run {run_id} started"));

            let (status_tx, status_rx) = mpsc::channel::<Result<AgentRunStatus, String>>();
            thread::spawn(move || {
                let mut after_sequence = 0;
                loop {
                    match send_ai_request(&AiIpcRequest::WatchAgentRun {
                        run_id: run_id.clone(),
                        after_sequence,
                    }) {
                        Ok(AiIpcResponse::AgentRunEvents { events, .. }) => {
                            if let Some(event) = events.last() {
                                after_sequence = event.sequence;
                            }
                        }
                        Ok(AiIpcResponse::Error { message }) => {
                            let _ = status_tx.send(Err(message));
                            break;
                        }
                        Ok(other) => {
                            let _ = status_tx
                                .send(Err(format!("unexpected AI watch response: {other:?}")));
                            break;
                        }
                        Err(error) => {
                            let _ = status_tx.send(Err(error.to_string()));
                            break;
                        }
                    }
                    let status = match send_ai_request(&AiIpcRequest::GetAgentRun {
                        run_id: run_id.clone(),
                    }) {
                        Ok(AiIpcResponse::AgentRun {
                            status: Some(status),
                            ..
                        }) => status,
                        Ok(AiIpcResponse::AgentRun { status: None, .. }) => {
                            let _ = status_tx.send(Err("agent run disappeared".into()));
                            break;
                        }
                        Ok(AiIpcResponse::Error { message }) => {
                            let _ = status_tx.send(Err(message));
                            break;
                        }
                        Ok(other) => {
                            let _ =
                                status_tx.send(Err(format!("unexpected AI response: {other:?}")));
                            break;
                        }
                        Err(error) => {
                            let _ = status_tx.send(Err(error.to_string()));
                            break;
                        }
                    };
                    let done = status.state.is_terminal()
                        || status.state == AgentRunState::AwaitingConfirmation;
                    if status_tx.send(Ok(status)).is_err() || done {
                        break;
                    }
                }
            });

            let page_for_status = page.clone();
            let log_for_status = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || {
                match status_rx.try_recv() {
                    Ok(Ok(status)) => {
                        page_for_status.status.set_text(&format!(
                            "{} · steps {}/{}",
                            status.state.as_str(),
                            status.completed_tool_steps,
                            status.max_tool_steps
                        ));
                        let done = status.state.is_terminal()
                            || status.state == AgentRunState::AwaitingConfirmation;
                        if let Some(response) = status.result {
                            page_for_status
                                .result_buffer
                                .set_text(&render_agent_response_text(&response));
                            if let Some(confirmation) = response.confirmation {
                                *page_for_status.pending_plan_id.borrow_mut() =
                                    Some(confirmation.plan_id);
                                page_for_status.approve_button.set_sensitive(true);
                                page_for_status.deny_button.set_sensitive(true);
                                page_for_status
                                    .status
                                    .set_text("Action proposed · awaiting explicit review");
                            }
                        } else if let Some(error) = status.error.as_deref() {
                            page_for_status
                                .result_buffer
                                .set_text(&format!("Desktop agent failed\n\n{error}"));
                        } else {
                            page_for_status
                                .result_buffer
                                .set_text(&render_agent_run_timeline(&status));
                        }
                        if done {
                            page_for_status.run_button.set_sensitive(true);
                            if status.state.is_terminal() {
                                page_for_status.cancel_button.set_sensitive(false);
                                page_for_status.active_run_id.borrow_mut().take();
                            }
                            append_log(&log_for_status, "[agent] run reached a stable state");
                            ControlFlow::Break
                        } else {
                            ControlFlow::Continue
                        }
                    }
                    Ok(Err(message)) => {
                        page_for_status
                            .status
                            .set_text(&format!("Agent status failed: {message}"));
                        page_for_status.run_button.set_sensitive(true);
                        page_for_status.cancel_button.set_sensitive(false);
                        ControlFlow::Break
                    }
                    Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                    Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
                }
            });
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            page.result_buffer
                .set_text(&format!("Desktop agent failed\n\n{message}"));
            page.status.set_text("Agent request failed");
            page.run_button.set_sensitive(true);
            append_log(&log_buffer, &format!("[agent] request failed: {message}"));
            ControlFlow::Break
        }
        Ok(Ok(other)) => {
            let message = format!("unexpected AI response: {other:?}");
            page.result_buffer.set_text(&message);
            page.status.set_text("Agent request failed");
            page.run_button.set_sensitive(true);
            append_log(&log_buffer, &format!("[agent] {message}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            page.result_buffer
                .set_text("Desktop agent failed\n\nThe response channel disconnected.");
            page.status.set_text("Agent request failed");
            page.run_button.set_sensitive(true);
            append_log(&log_buffer, "[agent] response channel disconnected");
            ControlFlow::Break
        }
    });
}

fn resolve_desktop_agent_action(
    page: Rc<DesktopAgentPage>,
    approved: bool,
    log_buffer: TextBuffer,
) {
    let Some(plan_id) = page.pending_plan_id.borrow_mut().take() else {
        return;
    };
    page.approve_button.set_sensitive(false);
    page.deny_button.set_sensitive(false);
    page.run_button.set_sensitive(false);
    page.status.set_text(if approved {
        "Waiting for native one-shot confirmation…"
    } else {
        "Denying proposed action…"
    });

    let (tx, rx) = mpsc::channel::<Result<AiIpcResponse, String>>();
    thread::spawn(move || {
        let _ = tx.send(
            send_ai_request(&AiIpcRequest::ConfirmAgentAction { plan_id, approved })
                .map_err(|error| error.to_string()),
        );
    });

    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiIpcResponse::AgentAction { response })) => {
            let result = response
                .result
                .as_ref()
                .map(pretty_json)
                .unwrap_or_else(|| "(no result payload)".to_string());
            append_to_text_buffer(
                &page.result_buffer,
                &format!(
                    "\n\nAction resolution\n-----------------\nTool: {}\nExecuted: {}\nResult: {}",
                    response.tool, response.executed, result
                ),
            );
            page.status.set_text(if response.executed {
                "Action executed after native confirmation"
            } else {
                "Action denied · nothing was changed"
            });
            page.run_button.set_sensitive(true);
            append_log(
                &log_buffer,
                if response.executed {
                    "[agent] confirmed action executed"
                } else {
                    "[agent] proposed action denied"
                },
            );
            ControlFlow::Break
        }
        Ok(Ok(AiIpcResponse::Error { message })) | Ok(Err(message)) => {
            append_to_text_buffer(
                &page.result_buffer,
                &format!("\n\nAction resolution failed\n------------------------\n{message}"),
            );
            page.status.set_text("Action was not executed");
            page.run_button.set_sensitive(true);
            append_log(
                &log_buffer,
                &format!("[agent] action resolution failed: {message}"),
            );
            ControlFlow::Break
        }
        Ok(Ok(other)) => {
            let message = format!("unexpected AI response: {other:?}");
            append_to_text_buffer(&page.result_buffer, &format!("\n\n{message}"));
            page.status.set_text("Action was not executed");
            page.run_button.set_sensitive(true);
            append_log(&log_buffer, &format!("[agent] {message}"));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            append_to_text_buffer(
                &page.result_buffer,
                "\n\nAction resolution failed: response channel disconnected",
            );
            page.status.set_text("Action was not executed");
            page.run_button.set_sensitive(true);
            append_log(&log_buffer, "[agent] action response channel disconnected");
            ControlFlow::Break
        }
    });
}

fn render_agent_response_text(response: &AgentResponse) -> String {
    let mut rendered = format!(
        "Desktop Agent\n=============\nProvider: {}\nModel: {}\n\nAnswer\n------\n{}",
        response.provider,
        response.model.as_deref().unwrap_or("default"),
        response.answer.trim()
    );

    if !response.steps.is_empty() {
        rendered.push_str("\n\nTool trace\n----------");
        for (index, step) in response.steps.iter().enumerate() {
            rendered.push_str(&format!(
                "\n{}. {}\n   Arguments: {}\n   Result: {}",
                index + 1,
                step.tool,
                pretty_json(&step.arguments),
                pretty_json(&step.result)
            ));
        }
    }

    if let Some(usage) = response.usage {
        rendered.push_str(&format!(
            "\n\nUsage\n-----\nInput tokens: {}\nOutput tokens: {}",
            usage.input_tokens, usage.output_tokens
        ));
    }

    if let Some(confirmation) = response.confirmation.as_ref() {
        rendered.push_str(&format!(
            "\n\nProposed action — NOT EXECUTED\n------------------------------\nTool: {}\nArguments: {}\nApproval expires: {}",
            confirmation.tool,
            pretty_json(&confirmation.arguments),
            confirmation.expires_at_unix
        ));
    }

    rendered
}

fn render_agent_run_timeline(status: &AgentRunStatus) -> String {
    let mut rendered = String::from("Agent timeline\n==============");
    for event in &status.events {
        let line = match &event.kind {
            AgentRunEventKind::Registered => "Run registered".to_string(),
            AgentRunEventKind::Triggered {
                trigger_id,
                trigger_kind,
            } => format!("Triggered by {} ({})", trigger_id, trigger_kind.as_str()),
            AgentRunEventKind::Retried {
                source_run_id,
                recovered_steps,
            } => format!("Retried {source_run_id} from {recovered_steps} safe observations"),
            AgentRunEventKind::PermissionRequested => "Waiting for permission".to_string(),
            AgentRunEventKind::Queued => "Queued for execution".to_string(),
            AgentRunEventKind::Planning { iteration } => {
                format!("Planning iteration {iteration}")
            }
            AgentRunEventKind::ToolStarted { step, tool } => {
                format!("Step {step}: running {tool}")
            }
            AgentRunEventKind::ToolCompleted { step, tool } => {
                format!("Step {step}: observed {tool}")
            }
            AgentRunEventKind::ActionProposed { tool } => {
                format!("Proposed action: {tool}")
            }
            AgentRunEventKind::AwaitingConfirmation => "Awaiting confirmation".to_string(),
            AgentRunEventKind::Completed => "Completed".to_string(),
            AgentRunEventKind::Cancelled => "Cancelled".to_string(),
            AgentRunEventKind::Failed { message } => format!("Failed: {message}"),
        };
        rendered.push_str(&format!("\n{}. {line}", event.sequence));
    }
    rendered
}

fn pretty_json(value: &serde_json::Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

fn append_to_text_buffer(buffer: &TextBuffer, text: &str) {
    let mut end = buffer.end_iter();
    buffer.insert(&mut end, text);
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
            add_message(parent, message, "user-card");
        } else {
            add_message(parent, message, "ai-card");
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

    let backend_combo = ChoiceDropDown::new();
    let model_label = Label::new(Some("Model"));
    model_label.set_xalign(0.0);
    model_label.add_css_class("mode-control-label");
    let model_combo = ChoiceDropDown::new();
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
    backend_group.append(&backend_combo.widget);
    backend_group.append(&model_label);
    backend_group.append(&model_combo.widget);

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
        if let Some(selected) = combo.selected_id() {
            {
                let mut state = state_clone.borrow_mut();
                state.app_state.active_provider = selected;
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
            if let Some(selected) = combo.selected_id() {
                let mut state = state_clone.borrow_mut();
                state.app_state.active_model = selected;
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
    backend_combo: &ChoiceDropDown,
    model_combo: &ChoiceDropDown,
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
    backend_combo: &ChoiceDropDown,
    providers: &[ProviderInfo],
    selected_provider: &str,
) {
    if providers.is_empty() {
        backend_combo.replace(
            vec![("unavailable".into(), "No providers available".into())],
            "unavailable",
        );
        return;
    }

    let selected = if providers
        .iter()
        .any(|provider| provider.id == selected_provider)
    {
        selected_provider
    } else if let Some(default_provider) = providers.first() {
        &default_provider.id
    } else {
        "unavailable"
    };
    backend_combo.replace(
        providers
            .iter()
            .map(|provider| (provider.id.clone(), provider_label(provider)))
            .collect(),
        selected,
    );
}

fn populate_model_combo(
    model_combo: &ChoiceDropDown,
    runtime: &AiConsoleRuntime,
    selected_provider: &str,
    selected_model: &str,
) {
    let models = provider_models_for(runtime, selected_provider);
    if models.is_empty() {
        model_combo.replace(
            vec![("unavailable".into(), "No models listed".into())],
            "unavailable",
        );
        return;
    }

    let selected =
        if !selected_model.is_empty() && models.iter().any(|model| model.id == selected_model) {
            selected_model.to_owned()
        } else if let Some(default_model) = runtime
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
            default_model
        } else if let Some(first_model) = provider_models_for(runtime, selected_provider).first() {
            first_model.id.clone()
        } else {
            "unavailable".into()
        };
    model_combo.replace(
        models
            .into_iter()
            .map(|model| (model.id.clone(), model_label(&model)))
            .collect(),
        &selected,
    );
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

fn provider_health(telemetry: &ProviderTelemetry) -> &'static str {
    if telemetry.requests == 0 {
        "idle"
    } else if telemetry.last_failure_at_unix > telemetry.last_success_at_unix {
        "degraded"
    } else {
        "healthy"
    }
}

fn provider_telemetry_text(telemetry: Option<&ProviderTelemetry>) -> String {
    let Some(telemetry) = telemetry else {
        return "Health: telemetry unavailable".to_string();
    };
    let average_latency = telemetry
        .total_latency_ms
        .checked_div(telemetry.requests)
        .unwrap_or(0);
    let mut text = format!(
        "Health: {}\nRequests: {} ({} succeeded, {} failed, {} cancelled)\nRetries: {} · Timeouts: {} · Average latency: {} ms\nTraffic: {} bytes in / {} bytes out\nReported tokens: {} in / {} out",
        provider_health(telemetry),
        telemetry.requests,
        telemetry.successes,
        telemetry.failures,
        telemetry.cancellations,
        telemetry.retries,
        telemetry.timeouts,
        average_latency,
        telemetry.input_bytes,
        telemetry.output_bytes,
        telemetry.input_tokens,
        telemetry.output_tokens,
    );
    if let Some(error) = telemetry.last_error.as_deref() {
        text.push_str(&format!("\nLast error: {error}"));
    }
    text
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
        runtime_snapshot
            .status
            .as_ref()
            .map(|status| {
                let retries = status
                    .provider_telemetry
                    .iter()
                    .map(|telemetry| telemetry.retries)
                    .sum::<u64>();
                let failures = status
                    .provider_telemetry
                    .iter()
                    .map(|telemetry| telemetry.failures)
                    .sum::<u64>();
                format!("Provider retries: {retries} · failures: {failures}")
            })
            .unwrap_or_else(|| "Provider telemetry unavailable".to_string()),
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
        let telemetry = runtime_snapshot.status.as_ref().and_then(|status| {
            status
                .provider_telemetry
                .iter()
                .find(|telemetry| telemetry.provider == provider.id)
        });
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
            "Base URL: {}\n{}\n{}",
            provider.base_url.as_deref().unwrap_or("-"),
            model_text,
            provider_telemetry_text(telemetry),
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
    stream_controls: ChatStreamControls,
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
        let stream_controls = stream_controls.clone();
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
                stream_controls.clone(),
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

fn speak_local_async(text: String, log_buffer: TextBuffer) {
    let bounded = text.chars().take(3_500).collect::<String>();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_speech_request(&SpeechIpcRequest::interrupt(bounded)));
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(response)) if response.status != "error" => ControlFlow::Break,
        Ok(Ok(response)) => {
            append_log(
                &log_buffer,
                &format!(
                    "[ambient-voice] speech rejected: {}",
                    response.message.unwrap_or_else(|| "unknown error".into())
                ),
            );
            ControlFlow::Break
        }
        Ok(Err(error)) => {
            append_log(
                &log_buffer,
                &format!("[ambient-voice] speech unavailable: {error}"),
            );
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
    });
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

fn send_forget_request(id: MemoryId) -> anyhow::Result<()> {
    match send_ai_request(&AiIpcRequest::Forget { id })? {
        AiIpcResponse::Forgotten { id: forgotten } if forgotten == id => Ok(()),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn send_memory_status_request() -> anyhow::Result<MemoryStatus> {
    match send_ai_request(&AiIpcRequest::MemoryStatus)? {
        AiIpcResponse::MemoryStatus { status } => Ok(status),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn send_clear_memory_request() -> anyhow::Result<usize> {
    match send_ai_request(&AiIpcRequest::ClearMemory)? {
        AiIpcResponse::MemoryCleared { deleted } => Ok(deleted),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn send_indexed_documents_request() -> anyhow::Result<Vec<IndexedDocument>> {
    match send_ai_request(&AiIpcRequest::ListIndexedDocuments)? {
        AiIpcResponse::IndexedDocuments { documents } => Ok(documents),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn send_ingest_document_request(path: PathBuf) -> anyhow::Result<DocumentIngestResult> {
    match send_ai_request(&AiIpcRequest::IngestDocument { path })? {
        AiIpcResponse::DocumentIngested { result } => Ok(result),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn send_ingest_directory_request(
    path: PathBuf,
    recursive: bool,
) -> anyhow::Result<DirectoryIngestResult> {
    match send_ai_request(&AiIpcRequest::IngestDirectory { path, recursive })? {
        AiIpcResponse::DirectoryIngested { result } => Ok(result),
        AiIpcResponse::Error { message } => Err(anyhow::anyhow!(message)),
        other => Err(anyhow::anyhow!("unexpected AI response: {other:?}")),
    }
}

fn send_remove_document_request(source: String) -> anyhow::Result<bool> {
    match send_ai_request(&AiIpcRequest::RemoveIndexedDocument { source })? {
        AiIpcResponse::IndexedDocumentRemoved { removed, .. } => Ok(removed),
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
    stream_controls: ChatStreamControls,
    log_buffer: TextBuffer,
    prompt_text: String,
    source_label: &str,
    quick_prompts_page: Option<Rc<QuickPromptsPage>>,
) {
    if prompt_text.trim().is_empty() || !send_button.is_sensitive() {
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
    stream_controls.stop_button.set_sensitive(false);
    *stream_controls.active_request_id.borrow_mut() = None;
    entry.set_text("");
    append_log(
        &log_buffer,
        &format!(
            "[chat] {} request sent via provider {} ({})",
            source_label, provider, model
        ),
    );

    let (tx, rx) = mpsc::channel::<Result<AiStreamEvent, String>>();
    thread::spawn(move || {
        let event_tx = tx.clone();
        if let Err(err) = stream_ai_chat(request, move |event| {
            event_tx
                .send(Ok(event))
                .map_err(|_| anyhow::anyhow!("chat UI disconnected"))
        }) {
            let _ = tx.send(Err(err.to_string()));
        }
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
    let stream_controls_for_result = stream_controls.clone();
    let mut accumulated = String::new();
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(AiStreamEvent::Started { request_id, .. })) => {
            *stream_controls_for_result.active_request_id.borrow_mut() = Some(request_id);
            stream_controls_for_result.stop_button.set_sensitive(true);
            ControlFlow::Continue
        }
        Ok(Ok(AiStreamEvent::Delta { content, .. })) => {
            accumulated.push_str(&content);
            let mut store = state_for_result.borrow_mut();
            set_latest_ai_reply(&mut store, active_idx, &accumulated, "Streaming response");
            render_active_conversation(&chat_view_for_result, &store);
            if let Some(conversation) = store.conversations.get(active_idx).cloned() {
                render_conversation_panel(&detail_for_result, &conversation, "Active conversation");
            }
            ControlFlow::Continue
        }
        Ok(Ok(AiStreamEvent::Completed { response, .. })) => {
            let mut reply = response.content;
            if !response.citations.is_empty() {
                reply.push_str("\n\nSources:\n");
                for (index, citation) in response.citations.iter().enumerate() {
                    reply.push_str(&format!(
                        "[{}] {}\n",
                        index + 1,
                        citation.source.as_deref().unwrap_or("Local memory")
                    ));
                }
            }
            let mut store = state_for_result.borrow_mut();
            set_latest_ai_reply(&mut store, active_idx, &reply, "Recently updated");
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
            if source_label_for_result == "ambient voice+speech" {
                speak_local_async(reply.clone(), log_buffer_for_result.clone());
            }
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
            stream_controls_for_result.stop_button.set_sensitive(false);
            *stream_controls_for_result.active_request_id.borrow_mut() = None;
            entry_for_result.set_text("");
            ControlFlow::Break
        }
        Ok(Ok(AiStreamEvent::Failed { message, .. })) | Ok(Err(message)) => {
            let err = message;
            let error_message = format!("AI backend error: {err}");
            let mut store = state_for_result.borrow_mut();
            set_latest_ai_reply(&mut store, active_idx, &error_message, "Backend error");
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
            stream_controls_for_result.stop_button.set_sensitive(false);
            *stream_controls_for_result.active_request_id.borrow_mut() = None;
            entry_for_result.set_text("");
            ControlFlow::Break
        }
        Ok(Ok(AiStreamEvent::Cancelled { .. })) => {
            let reply = if accumulated.is_empty() {
                "[response cancelled]".to_string()
            } else {
                accumulated.clone()
            };
            let mut store = state_for_result.borrow_mut();
            set_latest_ai_reply(&mut store, active_idx, &reply, "Response cancelled");
            persist_state(&store);
            render_active_conversation(&chat_view_for_result, &store);
            if let Some(conversation) = store.conversations.get(active_idx).cloned() {
                render_conversation_panel(&detail_for_result, &conversation, "Active conversation");
            }
            append_log(&log_buffer_for_result, "[chat] response cancelled");
            if let Some(view) = quick_prompts_page_for_result.as_ref() {
                {
                    let mut prompt_state = view.state.borrow_mut();
                    prompt_state.last_response = None;
                    prompt_state.last_error = Some("response cancelled".to_string());
                    prompt_state.in_flight = false;
                }
                refresh_quick_prompts_page_view(view.clone());
            }
            send_button_for_result.set_sensitive(true);
            stream_controls_for_result.stop_button.set_sensitive(false);
            *stream_controls_for_result.active_request_id.borrow_mut() = None;
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
            stream_controls_for_result.stop_button.set_sensitive(false);
            *stream_controls_for_result.active_request_id.borrow_mut() = None;
            ControlFlow::Break
        }
    });
}

fn set_latest_ai_reply(store: &mut PersistedState, active_idx: usize, reply: &str, summary: &str) {
    if let Some(conversation) = store.conversations.get_mut(active_idx) {
        if let Some(last_message) = conversation.messages.last_mut() {
            *last_message = format!("AI: {reply}");
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
        use_memory: store.app_state.use_memory,
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

fn memory_status_lines(status: &MemoryStatus) -> Vec<String> {
    vec![
        format!("AI memory records: {}", status.entry_count),
        format!("Vector backend: {}", status.vector_backend),
        format!("Storage schema: v{}", status.schema_version),
        format!(
            "Retention: {}",
            status
                .retention_days
                .map(|days| format!("{days} days"))
                .unwrap_or_else(|| "disabled".to_string())
        ),
        format!(
            "Capacity: {}",
            status
                .max_entries
                .map(|entries| format!("{entries} records"))
                .unwrap_or_else(|| "unlimited".to_string())
        ),
    ]
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
    let recall_results = Box::new(Orientation::Vertical, 6);

    let lifecycle_box = Box::new(Orientation::Vertical, 6);
    lifecycle_box.append(&note_card("Loading memory lifecycle policy..."));
    page.append(&lifecycle_box);

    {
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(send_memory_status_request());
        });
        let lifecycle_box = lifecycle_box.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
            Ok(Ok(status)) => {
                clear_box(&lifecycle_box);
                lifecycle_box.append(&info_card(&memory_status_lines(&status)));
                ControlFlow::Break
            }
            Ok(Err(err)) => {
                clear_box(&lifecycle_box);
                lifecycle_box.append(&note_card(&format!("Memory status unavailable: {err}")));
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
        });
    }

    let clear_all = Button::with_label("Clear all AI memory");
    clear_all.add_css_class("sidebar-button");
    {
        let clear_button = clear_all.clone();
        let store = store.clone();
        let notes_box = notes_box.clone();
        let recall_results = recall_results.clone();
        let lifecycle_box = lifecycle_box.clone();
        let log_buffer = log_buffer.clone();
        clear_all.connect_clicked(move |_| {
            clear_button.set_sensitive(false);
            append_log(&log_buffer, "[memory] bulk deletion requested");
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let result = send_clear_memory_request().and_then(|deleted| {
                    send_memory_status_request().map(|status| (deleted, status))
                });
                let _ = tx.send(result);
            });

            let clear_button = clear_button.clone();
            let store = store.clone();
            let notes_box = notes_box.clone();
            let recall_results = recall_results.clone();
            let lifecycle_box = lifecycle_box.clone();
            let log_buffer = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok((deleted, status))) => {
                    {
                        let mut state = store.borrow_mut();
                        state.app_state.memory_notes.clear();
                        persist_state(&state);
                    }
                    clear_box(&notes_box);
                    clear_box(&recall_results);
                    clear_box(&lifecycle_box);
                    lifecycle_box.append(&info_card(&memory_status_lines(&status)));
                    append_log(
                        &log_buffer,
                        &format!("[memory] permanently deleted {deleted} memory record(s)"),
                    );
                    clear_button.set_sensitive(true);
                    ControlFlow::Break
                }
                Ok(Err(err)) => {
                    append_log(&log_buffer, &format!("[memory] clear failed: {err}"));
                    clear_button.set_sensitive(true);
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    append_log(&log_buffer, "[memory] clear request disconnected");
                    clear_button.set_sensitive(true);
                    ControlFlow::Break
                }
            });
        });
    }
    page.append(&clear_all);

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
                            recall_results_for_result
                                .append(&recall_hit_card(hit, log_buffer_for_result.clone()));
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

fn indexed_sources_page(log_buffer: TextBuffer) -> Box {
    let page = section_shell(
        "Indexed Sources",
        "Documents available to grounded chat through hybrid semantic and full-text retrieval",
    );
    page.append(&info_card(&[
        "Index UTF-8 text, Markdown, source code, PDF, or DOCX files.".to_string(),
        "Reindex replaces prior chunks when the file changes; removal never deletes the original file."
            .to_string(),
    ]));

    let source_entry = Entry::builder()
        .placeholder_text("/absolute/path/to/document.pdf")
        .hexpand(true)
        .build();
    let index_button = action_button("Index / Refresh");
    let folder_button = action_button("Add Folder");
    let refresh_button = action_button("Refresh List");
    let controls = Box::new(Orientation::Horizontal, 8);
    controls.append(&source_entry);
    controls.append(&index_button);
    controls.append(&folder_button);
    controls.append(&refresh_button);
    page.append(&controls);

    let operation_status = Label::new(Some("Choose a file or folder to index."));
    operation_status.set_xalign(0.0);
    operation_status.set_wrap(true);
    operation_status.add_css_class("source-status");
    page.append(&operation_status);

    let list = Box::new(Orientation::Vertical, 8);
    list.append(&note_card("Loading indexed sources..."));
    let scroll = ScrolledWindow::builder()
        .min_content_height(360)
        .vexpand(true)
        .child(&list)
        .build();
    page.append(&scroll);

    refresh_indexed_sources(list.clone(), log_buffer.clone());
    {
        let list = list.clone();
        let log_buffer = log_buffer.clone();
        let button = folder_button.clone();
        let status = operation_status.clone();
        folder_button.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title("Choose a folder to index")
                .accept_label("Index Folder")
                .modal(true)
                .build();
            let list = list.clone();
            let log_buffer = log_buffer.clone();
            let button = button.clone();
            let status = status.clone();
            dialog.select_folder(
                None::<&gtk4::Window>,
                gtk4::gio::Cancellable::NONE,
                move |selection| match selection {
                    Ok(folder) => {
                        let Some(path) = folder.path() else {
                            set_source_status(
                                &status,
                                "The selected folder is not a local directory.",
                                true,
                            );
                            return;
                        };
                        set_source_status(
                            &status,
                            &format!("Indexing {} recursively…", path.display()),
                            false,
                        );
                        let indexing_path = path.display().to_string();
                        let started_at = Instant::now();
                        button.set_sensitive(false);
                        let (tx, rx) = mpsc::channel();
                        thread::spawn(move || {
                            let _ = tx.send(send_ingest_directory_request(path, true));
                        });
                        let list = list.clone();
                        let log_buffer = log_buffer.clone();
                        let button = button.clone();
                        let status = status.clone();
                        let mut last_elapsed_second = 0;
                        glib::timeout_add_local(Duration::from_millis(50), move || {
                            match rx.try_recv() {
                                Ok(Ok(result)) => {
                                    button.set_sensitive(true);
                                    set_source_status(
                                        &status,
                                        &format!(
                                            "Folder complete: {} indexed, {} unchanged, {} skipped, {} failed ({} chunks).",
                                            result.indexed,
                                            result.unchanged,
                                            result.skipped,
                                            result.failed,
                                            result.chunks
                                        ),
                                        result.failed > 0,
                                    );
                                    append_log(
                                        &log_buffer,
                                        &format!(
                                            "[sources] folder indexed={} unchanged={} skipped={} failed={}",
                                            result.indexed,
                                            result.unchanged,
                                            result.skipped,
                                            result.failed
                                        ),
                                    );
                                    for error in &result.errors {
                                        append_log(&log_buffer, &format!("[sources] {error}"));
                                    }
                                    refresh_indexed_sources(list.clone(), log_buffer.clone());
                                    ControlFlow::Break
                                }
                                Ok(Err(error)) => {
                                    button.set_sensitive(true);
                                    set_source_status(
                                        &status,
                                        &format!("Folder indexing failed: {error}"),
                                        true,
                                    );
                                    append_log(
                                        &log_buffer,
                                        &format!("[sources] folder indexing failed: {error}"),
                                    );
                                    ControlFlow::Break
                                }
                                Err(mpsc::TryRecvError::Empty) => {
                                    let elapsed = started_at.elapsed().as_secs();
                                    if elapsed != last_elapsed_second {
                                        last_elapsed_second = elapsed;
                                        set_source_status(
                                            &status,
                                            &format!(
                                                "Indexing {indexing_path} recursively… {elapsed}s elapsed. Large projects can take several minutes."
                                            ),
                                            false,
                                        );
                                    }
                                    ControlFlow::Continue
                                }
                                Err(mpsc::TryRecvError::Disconnected) => {
                                    button.set_sensitive(true);
                                    set_source_status(
                                        &status,
                                        "Folder indexing failed: the background request stopped unexpectedly.",
                                        true,
                                    );
                                    ControlFlow::Break
                                }
                            }
                        });
                    }
                    Err(error)
                        if error.matches(gtk4::gio::IOErrorEnum::Cancelled) => {}
                    Err(error) => set_source_status(
                        &status,
                        &format!("Could not choose folder: {error}"),
                        true,
                    ),
                },
            );
        });
    }
    {
        let list = list.clone();
        let log_buffer = log_buffer.clone();
        refresh_button.connect_clicked(move |_| {
            refresh_indexed_sources(list.clone(), log_buffer.clone());
        });
    }
    {
        let list = list.clone();
        let log_buffer = log_buffer.clone();
        let entry = source_entry.clone();
        let button = index_button.clone();
        let status = operation_status.clone();
        index_button.connect_clicked(move |_| {
            let path = entry.text().trim().to_string();
            if path.is_empty() {
                set_source_status(&status, "Enter an absolute file path first.", true);
                return;
            }
            set_source_status(&status, &format!("Indexing {path}…"), false);
            button.set_sensitive(false);
            entry.set_sensitive(false);
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(send_ingest_document_request(PathBuf::from(path)));
            });
            let list = list.clone();
            let log_buffer = log_buffer.clone();
            let button = button.clone();
            let entry = entry.clone();
            let status = status.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(result)) => {
                    button.set_sensitive(true);
                    entry.set_sensitive(true);
                    entry.set_text("");
                    set_source_status(
                        &status,
                        &if result.unchanged {
                            format!(
                                "Already current: {} chunk(s) from {}.",
                                result.chunks, result.source
                            )
                        } else {
                            format!("Indexed {} chunk(s) from {}.", result.chunks, result.source)
                        },
                        false,
                    );
                    append_log(
                        &log_buffer,
                        &format!("[sources] indexed {} document chunk(s)", result.chunks),
                    );
                    refresh_indexed_sources(list.clone(), log_buffer.clone());
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    button.set_sensitive(true);
                    entry.set_sensitive(true);
                    set_source_status(&status, &format!("Indexing failed: {error}"), true);
                    append_log(&log_buffer, &format!("[sources] indexing failed: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => {
                    button.set_sensitive(true);
                    entry.set_sensitive(true);
                    set_source_status(
                        &status,
                        "Indexing failed: the background request stopped unexpectedly.",
                        true,
                    );
                    ControlFlow::Break
                }
            });
        });
    }
    page
}

fn refresh_indexed_sources(list: Box, log_buffer: TextBuffer) {
    while let Some(child) = list.first_child() {
        list.remove(&child);
    }
    list.append(&note_card("Loading indexed sources..."));
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(send_indexed_documents_request());
    });
    glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
        Ok(Ok(documents)) => {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            if documents.is_empty() {
                list.append(&note_card("No documents are indexed yet."));
            } else {
                for document in documents {
                    list.append(&indexed_document_card(
                        document,
                        list.clone(),
                        log_buffer.clone(),
                    ));
                }
            }
            ControlFlow::Break
        }
        Ok(Err(error)) => {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            list.append(&note_card(&format!(
                "Could not load indexed sources: {error}"
            )));
            ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
    });
}

fn indexed_document_card(document: IndexedDocument, list: Box, log_buffer: TextBuffer) -> Box {
    let card = Box::new(Orientation::Vertical, 6);
    card.add_css_class("item-card");
    let title = Label::new(Some(&document.title));
    title.set_xalign(0.0);
    title.add_css_class("item-title");
    card.append(&title);
    let details = Label::new(Some(&format!(
        "{}\n{} · {} chunks · indexed {}",
        document.source, document.media_type, document.chunk_count, document.indexed_at_unix
    )));
    details.set_xalign(0.0);
    details.set_wrap(true);
    details.add_css_class("item-meta");
    card.append(&details);
    let actions = Box::new(Orientation::Horizontal, 8);
    let reindex = action_button("Reindex");
    let remove = action_button("Remove");
    actions.append(&reindex);
    actions.append(&remove);
    card.append(&actions);

    {
        let source = document.source.clone();
        let list = list.clone();
        let log_buffer = log_buffer.clone();
        reindex.connect_clicked(move |button| {
            button.set_sensitive(false);
            let source = source.clone();
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(send_ingest_document_request(PathBuf::from(source)));
            });
            let button = button.clone();
            let list = list.clone();
            let log_buffer = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(_)) => {
                    button.set_sensitive(true);
                    refresh_indexed_sources(list.clone(), log_buffer.clone());
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    button.set_sensitive(true);
                    append_log(&log_buffer, &format!("[sources] reindex failed: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
            });
        });
    }
    {
        let source = document.source;
        let list = list.clone();
        remove.connect_clicked(move |button| {
            button.set_sensitive(false);
            let source = source.clone();
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(send_remove_document_request(source));
            });
            let button = button.clone();
            let list = list.clone();
            let log_buffer = log_buffer.clone();
            glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
                Ok(Ok(_)) => {
                    refresh_indexed_sources(list.clone(), log_buffer.clone());
                    ControlFlow::Break
                }
                Ok(Err(error)) => {
                    button.set_sensitive(true);
                    append_log(&log_buffer, &format!("[sources] removal failed: {error}"));
                    ControlFlow::Break
                }
                Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
                Err(mpsc::TryRecvError::Disconnected) => ControlFlow::Break,
            });
        });
    }
    card
}

fn settings_page(store: Rc<RefCell<PersistedState>>, log_buffer: TextBuffer) -> Box {
    let page = section_shell("Settings", "Console preferences");
    let snapshot = store.borrow().app_state.clone();
    page.append(&info_card(&[
        format!("Compact sidebar: {}", snapshot.compact_sidebar),
        format!("Show timestamps: {}", snapshot.show_timestamps),
        format!("Auto-scroll chat: {}", snapshot.auto_scroll),
        format!("Verbose output: {}", snapshot.verbose_output),
        format!("Use memory in chat: {}", snapshot.use_memory),
    ]));

    for (label, active) in [
        ("Compact sidebar", snapshot.compact_sidebar),
        ("Show timestamps", snapshot.show_timestamps),
        ("Auto-scroll chat", snapshot.auto_scroll),
        ("Verbose tool output", snapshot.verbose_output),
        ("Use memory in chat", snapshot.use_memory),
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

fn set_source_status(label: &Label, message: &str, is_error: bool) {
    label.set_text(message);
    if is_error {
        label.add_css_class("source-status-error");
    } else {
        label.remove_css_class("source-status-error");
    }
}

fn recall_hit_card(hit: &SearchHit, log_buffer: TextBuffer) -> Box {
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

    let forget = Button::with_label("Forget");
    forget.add_css_class("sidebar-button");
    let id = hit.record.id;
    let card_for_result = card.clone();
    let forget_for_click = forget.clone();
    forget.connect_clicked(move |_| {
        forget_for_click.set_sensitive(false);
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(send_forget_request(id));
        });

        let card = card_for_result.clone();
        let forget = forget_for_click.clone();
        let log_buffer = log_buffer.clone();
        glib::timeout_add_local(Duration::from_millis(50), move || match rx.try_recv() {
            Ok(Ok(())) => {
                card.set_visible(false);
                append_log(&log_buffer, &format!("[memory] forgot memory id {id}"));
                ControlFlow::Break
            }
            Ok(Err(err)) => {
                forget.set_sensitive(true);
                append_log(&log_buffer, &format!("[memory] forget failed: {err}"));
                ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                forget.set_sensitive(true);
                append_log(&log_buffer, "[memory] forget request disconnected");
                ControlFlow::Break
            }
        });
    });
    card.append(&forget);

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
            "Use memory in chat" => state.app_state.use_memory = value,
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

        .source-status {
            color: @fd_app_text_dim;
            font-size: 0.88em;
        }

        .source-status-error {
            color: @fd_app_red;
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
        state.app_state.active_conversation = 0;
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
        let app_state = AppState {
            active_conversation: 0,
            ..AppState::default()
        };
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

    #[test]
    fn chat_request_uses_memory_only_when_enabled() {
        let mut store = PersistedState::default();
        assert!(!build_chat_request(&store, 0, "current").use_memory);

        store.app_state.use_memory = true;
        assert!(build_chat_request(&store, 0, "current").use_memory);
    }

    #[test]
    fn desktop_agent_result_discloses_tool_trace_and_unexecuted_action() {
        let response = AgentResponse {
            run_id: "test-run".into(),
            provider: "test-provider".into(),
            model: Some("test-model".into()),
            answer: "The editor is on workspace 2.".into(),
            usage: None,
            steps: vec![focaldesk_ai::AgentStepResult {
                tool: "list_windows".into(),
                arguments: serde_json::json!({}),
                result: serde_json::json!({"windows": [{"id": 7}]}),
            }],
            proposed_action: Some(focaldesk_ai::AgentProposedAction {
                tool: "focus_window".into(),
                arguments: serde_json::json!({"window_id": 7}),
            }),
            confirmation: Some(focaldesk_ai::AgentConfirmation {
                plan_id: "a".repeat(48),
                expires_at_unix: 42,
                tool: "focus_window".into(),
                arguments: serde_json::json!({"window_id": 7}),
            }),
        };

        let rendered = render_agent_response_text(&response);
        assert!(rendered.contains("Provider: test-provider"));
        assert!(rendered.contains("1. list_windows"));
        assert!(rendered.contains("\"windows\""));
        assert!(rendered.contains("Proposed action — NOT EXECUTED"));
        assert!(rendered.contains("Tool: focus_window"));
        assert!(rendered.contains("\"window_id\": 7"));
        assert!(!rendered.contains(&"a".repeat(48)));
    }

    #[test]
    fn coding_agent_registry_has_unique_stable_ids_and_commands() {
        let mut ids = std::collections::BTreeSet::new();
        let mut commands = std::collections::BTreeSet::new();
        for agent in CODING_AGENTS {
            assert!(ids.insert(agent.id));
            assert!(commands.insert(agent.command));
            assert!(!agent.label.is_empty());
        }
        assert!(coding_agent(&default_coding_agent()).is_some());
    }

    #[test]
    fn coding_agent_search_includes_user_cli_install_roots() {
        let home = Path::new("/home/tester");
        let directories =
            executable_search_dirs(Some(std::ffi::OsStr::new("/usr/bin")), Some(home));

        assert_eq!(directories.first(), Some(&PathBuf::from("/usr/bin")));
        assert!(directories.contains(&home.join(".local/bin")));
        assert!(directories.contains(&home.join(".cargo/bin")));
        assert!(directories.contains(&home.join(".npm-global/bin")));
        assert!(directories.contains(&home.join(".local/share/pnpm")));
    }

    #[test]
    fn coding_agent_uses_weston_terminal_shell_option() {
        let executable = Path::new("/home/tester/.npm-global/bin/codex");
        assert_eq!(
            coding_agent_terminal_args("weston-terminal", executable),
            vec![OsString::from("--shell=/home/tester/.npm-global/bin/codex")]
        );
        assert_eq!(
            coding_agent_terminal_args("/usr/bin/foot", executable),
            vec![OsString::from("-e"), executable.as_os_str().into()]
        );
    }

    #[test]
    fn coding_agent_prefers_existing_work_directory() {
        let temp = std::env::temp_dir().join(format!(
            "focaldesk-ai-console-workdir-{}",
            std::process::id()
        ));
        let home = temp.join("home");
        let work = home.join("Work");
        fs::create_dir_all(&work).unwrap();

        assert_eq!(preferred_coding_agent_workdir(&home), work);
        fs::remove_dir_all(temp).unwrap();
    }

    #[test]
    fn scenario_lab_example_is_a_safe_deterministic_contract() {
        let fixture: focaldesk_ai::ScenarioFixture =
            serde_json::from_str(scenario_example_json()).unwrap();
        let report = focaldesk_ai::evaluate_scenario(fixture).unwrap();
        assert!(report.passed, "{:#?}", report.unexpected_violation_codes);
        assert_eq!(report.provider_calls, 0);
        assert_eq!(report.tool_executions, 0);
        assert_eq!(report.live_mutations, 0);
    }
}
