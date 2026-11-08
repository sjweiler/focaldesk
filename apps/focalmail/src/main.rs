mod attachments;
mod drafts;
#[path = "gmail.rs"]
mod gmail_backend;
mod imap_backend;
mod mime;
mod notifications;
mod outlook;
mod providers;
mod service;
use service::client as gmail;
mod html;
mod settings;
mod store;
use eframe::egui::{self, Color32, RichText};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

const BLUE: Color32 = Color32::from_rgb(0, 174, 239);
const PANEL: Color32 = Color32::from_rgb(43, 43, 43);
const READER: Color32 = Color32::from_rgb(33, 36, 32);
const TEXT: Color32 = Color32::from_rgb(235, 237, 239);
const MUTED: Color32 = Color32::from_rgb(166, 173, 179);

fn avatar(ui: &mut egui::Ui, name: &str, size: f32) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let colors = [
        Color32::from_rgb(65, 122, 155),
        Color32::from_rgb(111, 115, 72),
        Color32::from_rgb(130, 91, 119),
    ];
    let index = name.bytes().map(usize::from).sum::<usize>() % colors.len();
    ui.painter()
        .circle_filled(rect.center(), size / 2.0, colors[index]);
    let initials: String = name
        .split_whitespace()
        .take(2)
        .filter_map(|s| s.chars().next())
        .collect();
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        initials,
        egui::FontId::proportional(size * 0.36),
        Color32::WHITE,
    );
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Message {
    id: String,
    sender: String,
    email: String,
    subject: String,
    body: String,
    date: String,
    account: String,
    folder: String,
    unread: bool,
    flagged: bool,
    #[serde(default)]
    labels: Vec<String>,
    #[serde(default)]
    timestamp: i64,
    #[serde(default)]
    attachments: Vec<attachments::Attachment>,
    #[serde(default)]
    attachments_loaded: bool,
    #[serde(default)]
    html: Option<html::Document>,
    #[serde(default)]
    html_loaded: bool,
    #[serde(default)]
    to_header: String,
    #[serde(default)]
    cc_header: String,
    #[serde(default)]
    reply_to: String,
    #[serde(default)]
    delivered_to: String,
    #[serde(default)]
    message_id: String,
    #[serde(default)]
    references: String,
    #[serde(default)]
    thread_id: String,
    #[serde(default)]
    recipients_loaded: bool,
}

impl Message {
    fn update_content(&mut self, loaded: &Message) {
        self.body = loaded.body.clone();
        self.html = loaded.html.clone();
        self.html_loaded = loaded.html_loaded;
        self.attachments = loaded.attachments.clone();
        self.attachments_loaded = loaded.attachments_loaded;
        self.to_header = loaded.to_header.clone();
        self.cc_header = loaded.cc_header.clone();
        self.reply_to = loaded.reply_to.clone();
        self.delivered_to = loaded.delivered_to.clone();
        self.message_id = loaded.message_id.clone();
        self.references = loaded.references.clone();
        self.thread_id = loaded.thread_id.clone();
        self.recipients_loaded = loaded.recipients_loaded;
    }
    fn in_folder(&self, folder: &str) -> bool {
        if self.labels.is_empty() {
            return self.folder == folder;
        }
        let hidden = self
            .labels
            .iter()
            .any(|l| matches!(l.as_str(), "TRASH" | "SPAM" | "DRAFT"));
        match folder {
            "Inbox" => !hidden && self.labels.iter().any(|l| l == "INBOX"),
            "Sent" => !hidden && self.labels.iter().any(|l| l == "SENT"),
            _ => self.folder == folder,
        }
    }
}

enum FileEvent {
    Imported(attachments::DraftAttachment),
    Downloaded(String),
    Content(Box<Message>),
    Compose(Box<Message>, drafts::Kind),
    Forwarded(Vec<attachments::DraftAttachment>),
}

enum WorkerEvent {
    Progress(String),
    Finished(Result<String, String>),
}

struct Mail {
    messages: Vec<Message>,
    selected: Option<usize>,
    folder: String,
    query: String,
    contacts: bool,
    compose: bool,
    to: String,
    subject: String,
    body: String,
    status: String,
    account_notice: String,
    settings: settings::Settings,
    oauth_path: String,
    receiver: Option<mpsc::Receiver<WorkerEvent>>,
    cancel: Arc<AtomicBool>,
    live: bool,
    cache_ready: bool,
    accounts: Vec<String>,
    from: String,
    sending: bool,
    next_cache_load: std::time::Instant,
    files: Vec<attachments::DraftAttachment>,
    file_receiver: Option<mpsc::Receiver<anyhow::Result<FileEvent>>>,
    picker: attachments::Picker,
    formatted: bool,
    pending_link: Option<String>,
    cc: String,
    draft_state: drafts::Draft,
    saved_drafts: Vec<drafts::Summary>,
    show_drafts: bool,
    forward_selected: std::collections::BTreeSet<usize>,
}

impl Default for Mail {
    fn default() -> Self {
        let samples = [
            ("Bob Dylan", "bob@example.com", "Long time no see!", "Hi Alice,\n\nHope you're doing well! About our picnic — we'll be meeting at the park around 2PM. Looking forward to seeing you!\n\nBest regards,\n\nBob", "5:23 PM", "Gmail"),
            ("FocalDesk Team", "team@example.com", "Welcome to FocalMail", "Your mail, at home on FocalDesk.\n\nThis preview explores a familiar, focused workspace for your Gmail and Microsoft 365 accounts. All mail shown here is sample data.", "4:10 PM", "Microsoft 365"),
            ("Cristine", "cristine@example.com", "Invitation: Next release meeting", "Hi Alice,\n\nCan we review the next release on Tuesday at 1PM?\n\nThanks,\nCristine", "3:42 PM", "Microsoft 365"),
            ("Vox's Today Explained", "news@example.com", "The shape of things to come", "Your daily briefing.\n\nToday we're exploring the ideas shaping tomorrow's cities, communities, and technology.", "12:24 PM", "Gmail"),
            ("Ann Handley", "ann@example.com", "A few things worth sharing", "Hello!\n\nThree examples of creativity in unexpected places. A little inspiration for your afternoon.", "11:08 AM", "Gmail"),
            ("The Marginalian", "letters@example.com", "The art of paying attention", "A Sunday letter about curiosity, creativity, and the quiet beauty of everyday life.", "Yesterday", "Gmail"),
            ("New Yorker Science & Tech", "science@example.com", "What is the opposite of oil drilling?", "This week's science and technology reading list.\n\nA closer look at the changing world around us.", "Yesterday", "Gmail"),
        ];
        Self {
            messages: samples
                .into_iter()
                .enumerate()
                .map(
                    |(i, (sender, email, subject, body, date, account))| Message {
                        id: format!("demo-{i}"),
                        sender: sender.into(),
                        email: email.into(),
                        subject: subject.into(),
                        body: body.into(),
                        date: date.into(),
                        account: account.into(),
                        folder: "Inbox".into(),
                        unread: i != 0,
                        flagged: false,
                        labels: Vec::new(),
                        timestamp: 0,
                        attachments: Vec::new(),
                        attachments_loaded: false,
                        html: None,
                        html_loaded: false,
                        to_header: String::new(),
                        cc_header: String::new(),
                        reply_to: String::new(),
                        delivered_to: String::new(),
                        message_id: String::new(),
                        references: String::new(),
                        thread_id: String::new(),
                        recipients_loaded: true,
                    },
                )
                .collect(),
            selected: Some(0),
            folder: "All Inboxes".into(),
            query: String::new(),
            contacts: true,
            compose: false,
            to: String::new(),
            subject: String::new(),
            body: String::new(),
            status: "Demo • sample mail • no accounts connected".into(),
            account_notice: String::new(),
            settings: settings::Settings::default(),
            oauth_path: std::env::var("FOCALMAIL_GOOGLE_CLIENT_JSON").unwrap_or_default(),
            receiver: None,
            cancel: Arc::new(AtomicBool::new(false)),
            live: false,
            cache_ready: false,
            accounts: Vec::new(),
            from: String::new(),
            sending: false,
            next_cache_load: std::time::Instant::now(),
            files: Vec::new(),
            file_receiver: None,
            picker: attachments::Picker::default(),
            formatted: true,
            pending_link: None,
            cc: String::new(),
            draft_state: drafts::Draft::new(String::new()),
            saved_drafts: Vec::new(),
            show_drafts: false,
            forward_selected: std::collections::BTreeSet::new(),
        }
    }
}

impl Mail {
    fn open() -> Self {
        let mut mail = Self::default();
        mail.messages.clear();
        mail.selected = None;
        mail.live = true;
        mail.settings = settings::Settings::load();
        match store::Store::open().and_then(|store| {
            let messages = store.load()?;
            let accounts = store.accounts()?;
            mail.saved_drafts = store.list_drafts()?;
            if let Some(id) = store
                .active_draft_id()?
                .or_else(|| mail.saved_drafts.first().map(|d| d.id.clone()))
            {
                let (draft, files) = store.load_local_draft(&id)?;
                mail.apply_draft(draft, files);
                mail.compose = false;
            }
            Ok((messages, accounts))
        }) {
            Ok((messages, accounts)) => {
                mail.messages = messages;
                mail.cache_ready = true;
                mail.status = if accounts.is_empty() {
                    "Add an account in Settings → Accounts to get started".into()
                } else {
                    format!(
                        "Mail service syncs in background · {} account(s)",
                        accounts.len()
                    )
                };
                if !accounts.contains(&mail.from) {
                    mail.from = accounts.first().cloned().unwrap_or_default();
                }
                mail.accounts = accounts;
            }
            Err(e) => mail.status = format!("Could not open local cache: {e}"),
        }
        mail
    }

    fn start(&mut self, ctx: &egui::Context, connect: bool) {
        self.start_account(ctx, connect, None);
    }
    fn start_account(&mut self, ctx: &egui::Context, connect: bool, reconnect: Option<String>) {
        if self.receiver.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let path = self.oauth_path.clone();
        let ctx = ctx.clone();
        self.receiver = Some(rx);
        self.status = if connect || reconnect.is_some() {
            "Finish account sign-in in your browser…"
        } else {
            "Syncing mailboxes… cached messages remain available"
        }
        .into();
        std::thread::spawn(move || {
            let result = if let Some(email) = reconnect {
                gmail::reconnect(&email, cancel, |status| {
                    let _ = tx.send(WorkerEvent::Progress(status.into()));
                    ctx.request_repaint();
                })
            } else if connect {
                gmail::connect(&path, cancel, |status| {
                    let _ = tx.send(WorkerEvent::Progress(status.into()));
                    ctx.request_repaint();
                })
            } else {
                gmail::sync_all(cancel)
            };
            let _ = tx.send(WorkerEvent::Finished(result.map_err(|e| e.to_string())));
            ctx.request_repaint();
        });
    }

    fn start_profile(&mut self, ctx: &egui::Context, profile: settings::Profile, password: String) {
        if self.receiver.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        self.receiver = Some(rx);
        self.status = "Connecting mail account…".into();
        std::thread::spawn(move || {
            let result = gmail::connect_profile(profile, password, cancel, |status| {
                let _ = tx.send(WorkerEvent::Progress(status.into()));
                ctx.request_repaint();
            });
            let _ = tx.send(WorkerEvent::Finished(result.map_err(|e| e.to_string())));
            ctx.request_repaint();
        });
    }

    fn worker(
        &mut self,
        ctx: &egui::Context,
        task: impl FnOnce(Arc<AtomicBool>) -> anyhow::Result<String> + Send + 'static,
    ) {
        if self.receiver.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let ctx = ctx.clone();
        self.receiver = Some(rx);
        self.status = "Updating mail…".into();
        std::thread::spawn(move || {
            let _ = tx.send(WorkerEvent::Finished(
                task(cancel).map_err(|e| e.to_string()),
            ));
            ctx.request_repaint();
        });
    }
    fn action(&mut self, ctx: &egui::Context, action: gmail::Action) {
        if let Some(message) = self.selected.and_then(|i| self.messages.get(i)).cloned() {
            self.worker(ctx, move |cancel| gmail::modify(&message, action, cancel));
        }
    }

    fn file_task(
        &mut self,
        ctx: &egui::Context,
        task: impl FnOnce() -> anyhow::Result<FileEvent> + Send + 'static,
    ) {
        if self.file_receiver.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.file_receiver = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(task());
            ctx.request_repaint();
        });
    }
    fn import_attachment(&mut self, ctx: &egui::Context, path: std::path::PathBuf) {
        let remaining = attachments::MAX_BYTES
            .saturating_sub(self.files.iter().map(|f| f.data.len()).sum::<usize>());
        self.file_task(ctx, move || {
            Ok(FileEvent::Imported(attachments::import(&path, remaining)?))
        });
    }
    fn persist_attachments(&mut self) {
        self.save_current(true);
    }
    fn poll_files(&mut self) {
        let event = self
            .file_receiver
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(value) => Some(value),
                Err(mpsc::TryRecvError::Disconnected) => {
                    Some(Err(anyhow::anyhow!("Attachment worker stopped")))
                }
                Err(mpsc::TryRecvError::Empty) => None,
            });
        if let Some(event) = event {
            self.file_receiver = None;
            match event {
                Ok(FileEvent::Imported(file)) => {
                    self.status = format!("Attached {}", file.name);
                    self.files.push(file);
                    self.persist_attachments();
                }
                Ok(FileEvent::Downloaded(status)) => self.status = status,
                Ok(FileEvent::Content(loaded)) => {
                    for message in &mut self.messages {
                        if message.account == loaded.account && message.id == loaded.id {
                            message.update_content(&loaded);
                        }
                    }
                    self.status = "Message content loaded".into();
                }
                Ok(FileEvent::Compose(message, kind)) => {
                    for cached in &mut self.messages {
                        if cached.account == message.account && cached.id == message.id {
                            cached.update_content(&message);
                        }
                    }
                    self.begin_from(&message, kind);
                }
                Ok(FileEvent::Forwarded(files)) => {
                    self.files.extend(files);
                    self.forward_selected.clear();
                    self.persist_attachments();
                    self.status = "Selected forwarded attachments added to this draft".into();
                }
                Err(error) => self.status = format!("Message content: {error}"),
            }
        }
    }

    fn poll(&mut self) {
        let mut result = None;
        loop {
            let event = self.receiver.as_ref().map(|rx| rx.try_recv());
            match event {
                Some(Ok(WorkerEvent::Progress(status))) => {
                    self.status = status;
                    // Registration is independent of the potentially lengthy first sync.
                    if let Ok(accounts) = store::Store::open().and_then(|s| s.accounts()) {
                        self.accounts = accounts;
                        if !self.accounts.contains(&self.from) {
                            self.from = self.accounts.first().cloned().unwrap_or_default();
                        }
                    }
                }
                Some(Ok(WorkerEvent::Finished(value))) => {
                    result = Some(value);
                    break;
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    result = Some(Err("Mail worker stopped unexpectedly".into()));
                    break;
                }
                _ => break,
            }
        }
        if self.live && std::time::Instant::now() >= self.next_cache_load {
            self.next_cache_load = std::time::Instant::now() + std::time::Duration::from_secs(2);
            if let Ok(store) = store::Store::open() {
                if let Ok(accounts) = store.accounts() {
                    self.accounts = accounts;
                }
                if let Ok(states) = store.account_status() {
                    self.account_notice = states
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|s| s["state"] != "connected")
                        .map(|s| {
                            format!(
                                "{}: {}",
                                s["email"].as_str().unwrap_or(""),
                                s["state"].as_str().unwrap_or("offline").replace('_', " ")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(" · ");
                }
            }
            if let Ok(messages) = store::Store::open().and_then(|s| s.load()) {
                let selected = self
                    .selected
                    .and_then(|i| self.messages.get(i))
                    .map(|m| (m.account.clone(), m.id.clone()));
                self.messages = messages;
                self.selected = selected.and_then(|(account, id)| {
                    self.messages
                        .iter()
                        .position(|m| m.account == account && m.id == id)
                });
            }
        }
        if let Some(result) = result {
            self.receiver = None;
            if self.sending {
                if result.is_ok() {
                    self.to.clear();
                    self.subject.clear();
                    self.body.clear();
                    self.compose = false;
                    self.files.clear();
                    self.cc.clear();
                    self.draft_state = drafts::Draft::new(self.from.clone());
                    self.refresh_drafts();
                }
                self.sending = false;
            }
            self.status = match result {
                Ok(status) => status,
                Err(e) => format!("{e} · cached mail remains available"),
            };
            let selected = self
                .selected
                .and_then(|i| self.messages.get(i))
                .map(|m| (m.account.clone(), m.id.clone()));
            match store::Store::open().and_then(|s| Ok((s.load()?, s.accounts()?))) {
                Ok((messages, accounts)) => {
                    self.messages = messages;
                    self.accounts = accounts;
                    if !self.accounts.contains(&self.from) {
                        self.from = self.accounts.first().cloned().unwrap_or_default();
                    }
                    self.selected = selected.and_then(|(account, id)| {
                        self.messages
                            .iter()
                            .position(|m| m.account == account && m.id == id)
                    });
                }
                Err(e) => self.status = format!("{}; cannot reload cache: {e}", self.status),
            }
        }
    }
    fn matches(&self, m: &Message) -> bool {
        let folder = match self.folder.as_str() {
            "All Inboxes" => m.in_folder("Inbox"),
            "Unread" => m.unread && m.folder != "Trash",
            "Flagged" => m.flagged && m.folder != "Trash",
            "Gmail" => (self.live || m.account == self.folder) && m.in_folder("Inbox"),
            "Microsoft 365" => !self.live && m.account == self.folder && m.in_folder("Inbox"),
            other if self.accounts.iter().any(|account| account == other) => {
                m.account == other && m.in_folder("Inbox")
            }
            other => m.in_folder(other),
        };
        folder
            && format!("{} {} {}", m.sender, m.subject, m.body)
                .to_lowercase()
                .contains(&self.query.to_lowercase())
    }

    fn record(&self) -> drafts::Draft {
        let mut draft = self.draft_state.clone();
        draft.from = self.from.clone();
        draft.to = self.to.clone();
        draft.cc = self.cc.clone();
        draft.subject = self.subject.clone();
        draft.body = self.body.clone();
        draft
    }
    fn refresh_drafts(&mut self) {
        if self.cache_ready {
            match store::Store::open().and_then(|s| s.list_drafts()) {
                Ok(drafts) => self.saved_drafts = drafts,
                Err(e) => self.status = format!("Could not load drafts: {e}"),
            }
        }
    }
    fn save_current(&mut self, files: bool) -> bool {
        if !self.cache_ready {
            return true;
        }
        if !self.compose
            && self.to.is_empty()
            && self.cc.is_empty()
            && self.subject.is_empty()
            && self.body.is_empty()
            && self.files.is_empty()
        {
            return true;
        }
        let record = self.record();
        match store::Store::open().and_then(|mut s| {
            s.save_local_draft(&record, if files { Some(&self.files) } else { None })
        }) {
            Ok(()) => {
                self.refresh_drafts();
                true
            }
            Err(e) => {
                self.status = format!("Could not save current draft: {e}");
                false
            }
        }
    }
    fn apply_draft(&mut self, draft: drafts::Draft, files: Vec<attachments::DraftAttachment>) {
        self.from = draft.from.clone();
        self.to = draft.to.clone();
        self.cc = draft.cc.clone();
        self.subject = draft.subject.clone();
        self.body = draft.body.clone();
        self.draft_state = draft;
        self.files = files;
        self.forward_selected.clear();
        self.picker.open = false;
        self.compose = true;
    }
    fn open_saved_draft(&mut self, id: &str) {
        if self.sending || self.file_receiver.is_some() || !self.save_current(true) {
            return;
        }
        match store::Store::open().and_then(|s| s.load_local_draft(id)) {
            Ok((draft, files)) => self.apply_draft(draft, files),
            Err(e) => self.status = format!("Could not open draft: {e}"),
        }
    }
    fn begin_from(&mut self, message: &Message, kind: drafts::Kind) {
        if self.sending || !self.save_current(true) {
            return;
        }
        self.apply_draft(drafts::prepare(message, kind, &self.accounts), vec![]);
        self.save_current(true);
    }
    fn compose_action(&mut self, ctx: &egui::Context, kind: drafts::Kind) {
        if self.sending || self.file_receiver.is_some() {
            return;
        }
        if matches!(kind, drafts::Kind::New) {
            self.draft(false);
            return;
        }
        if let Some(message) = self.selected.and_then(|i| self.messages.get(i)).cloned() {
            if !message.recipients_loaded || !message.attachments_loaded || !message.html_loaded {
                self.status = "Loading original message for reply or forward…".into();
                self.file_task(ctx, move || {
                    Ok(FileEvent::Compose(
                        Box::new(gmail::load_content(&message)?),
                        kind,
                    ))
                });
            } else {
                self.begin_from(&message, kind);
            }
        }
    }
    fn draft(&mut self, reply: bool) {
        if self.sending || self.file_receiver.is_some() {
            return;
        }
        if reply {
            if let Some(message) = self.selected.and_then(|i| self.messages.get(i)).cloned() {
                self.begin_from(&message, drafts::Kind::Reply);
            }
        } else if self.save_current(true) {
            self.apply_draft(drafts::Draft::new(self.from.clone()), vec![]);
            self.save_current(true);
        }
    }
    fn discard_current(&mut self) {
        if self.sending || self.file_receiver.is_some() {
            return;
        }
        if self.cache_ready {
            if let Err(e) =
                store::Store::open().and_then(|s| s.delete_local_draft(&self.draft_state.id))
            {
                self.status = format!("Could not discard draft: {e}");
                return;
            }
        }
        self.apply_draft(drafts::Draft::new(self.from.clone()), vec![]);
        self.compose = false;
        self.refresh_drafts();
    }
}

impl eframe::App for Mail {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        let before = (
            self.to.clone(),
            self.subject.clone(),
            self.body.clone(),
            self.from.clone(),
            self.cc.clone(),
        );
        self.poll();
        self.poll_files();
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
        if self.receiver.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if !self.account_notice.is_empty() {
            egui::TopBottomPanel::bottom("account_connection_status").show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(&self.account_notice);
                    if ui.button("Manage accounts").clicked() {
                        self.settings.open = true;
                    }
                });
            });
        }
        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::new().fill(PANEL).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("FocalMail").size(19.0).strong());
                    ui.add_space(90.0);
                    ui.add(
                        egui::TextEdit::singleline(&mut self.query)
                            .hint_text("Search mail")
                            .desired_width(390.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.toggle_value(&mut self.contacts, "Contact details");
                    });
                });
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add(
                            egui::Button::new(RichText::new("+  New").color(Color32::WHITE))
                                .fill(BLUE),
                        )
                        .clicked()
                    {
                        self.compose_action(ctx, drafts::Kind::New);
                    }
                    ui.add_space(24.0);
                    if ui
                        .add_enabled(true, egui::Button::new("Settings"))
                        .clicked()
                    {
                        self.settings.open = true;
                    }
                    if ui
                        .add_enabled(
                            self.receiver.is_none() && self.cache_ready,
                            egui::Button::new("Refresh"),
                        )
                        .clicked()
                    {
                        self.start(ctx, false);
                    }
                    if self.receiver.is_some() && ui.button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = "Cancelling…".into();
                    }
                    ui.separator();
                    let selected = self.selected;
                    ui.add_enabled_ui(selected.is_some(), |ui| {
                        for (label, kind) in [
                            ("Reply", drafts::Kind::Reply),
                            ("Reply all", drafts::Kind::ReplyAll),
                            ("Forward", drafts::Kind::Forward),
                        ] {
                            if ui
                                .add_enabled(
                                    !self.sending && self.file_receiver.is_none(),
                                    egui::Button::new(label),
                                )
                                .clicked()
                            {
                                self.compose_action(ctx, kind);
                            }
                        }
                        let available = self.receiver.is_none() && self.live;
                        if ui
                            .add_enabled(available, egui::Button::new("Mark read"))
                            .clicked()
                        {
                            self.action(ctx, gmail::Action::Read(true));
                        }
                        if ui
                            .add_enabled(available, egui::Button::new("Mark unread"))
                            .clicked()
                        {
                            self.action(ctx, gmail::Action::Read(false));
                        }
                        if ui
                            .add_enabled(available, egui::Button::new("Toggle star"))
                            .clicked()
                        {
                            if let Some(i) = selected {
                                self.action(ctx, gmail::Action::Star(!self.messages[i].flagged));
                            }
                        }
                        if ui
                            .add_enabled(available, egui::Button::new("Archive"))
                            .clicked()
                        {
                            self.action(ctx, gmail::Action::Archive);
                        }
                        if ui
                            .add_enabled(available, egui::Button::new("Trash"))
                            .clicked()
                        {
                            self.action(ctx, gmail::Action::Trash);
                        }
                    });
                });
            });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.small(format!(
                "{} · {} messages cached",
                self.status,
                self.messages.len()
            ));
        });
        egui::SidePanel::left("folders")
            .default_width(235.0)
            .width_range(170.0..=320.0)
            .frame(egui::Frame::new().fill(PANEL).inner_margin(12.0))
            .show(ctx, |ui| {
                ui.heading("Mail");
                ui.add_space(18.0);
                ui.weak("FAVORITES");
                ui.add_space(8.0);
                let mut folders: Vec<String> = [
                    "All Inboxes",
                    "Unread",
                    "Flagged",
                    "Gmail",
                    "Microsoft 365",
                    "Inbox",
                    "Sent",
                    "Drafts",
                    "Archive",
                    "Trash",
                    "Spam",
                    "Server Drafts",
                ]
                .into_iter()
                .map(String::from)
                .collect();
                folders.splice(4..4, self.accounts.clone());
                for folder in folders {
                    if self.live && folder == "Microsoft 365" {
                        continue;
                    }
                    if folder == "Gmail" || folder == "Inbox" {
                        ui.add_space(20.0);
                    }
                    let count = self
                        .messages
                        .iter()
                        .filter(|m| match folder.as_str() {
                            "All Inboxes" => m.in_folder("Inbox"),
                            "Unread" => m.unread && m.folder != "Trash",
                            "Flagged" => m.flagged && m.folder != "Trash",
                            "Gmail" => (self.live || m.account == folder) && m.in_folder("Inbox"),
                            "Microsoft 365" => {
                                !self.live && m.account == folder && m.in_folder("Inbox")
                            }
                            other if self.accounts.iter().any(|account| account == other) => {
                                m.account == folder && m.in_folder("Inbox")
                            }
                            _ => m.in_folder(&folder),
                        })
                        .count();
                    let count = if folder == "Drafts" {
                        self.saved_drafts.len()
                    } else {
                        count
                    };
                    if ui
                        .add_sized(
                            [ui.available_width(), 26.0],
                            egui::Button::new(format!("{folder}     {count}"))
                                .selected(self.folder == folder)
                                .frame(false),
                        )
                        .clicked()
                    {
                        if folder == "Drafts" {
                            self.refresh_drafts();
                            self.show_drafts = true;
                        }
                        self.folder = folder;
                        self.selected = None;
                    }
                    ui.add_space(2.0);
                }
            });
        if self.contacts && ctx.screen_rect().width() > 1100.0 {
            egui::SidePanel::right("contacts")
                .default_width(220.0)
                .width_range(190.0..=300.0)
                .frame(egui::Frame::new().fill(PANEL).inner_margin(16.0))
                .show(ctx, |ui| {
                    ui.heading("Contact Details");
                    ui.add_space(28.0);
                    if let Some(m) = self.selected.and_then(|i| self.messages.get(i)).cloned() {
                        ui.label(RichText::new(&m.sender).strong());
                        ui.add_space(12.0);
                        ui.weak(&m.email);
                        ui.weak(&m.account);
                        ui.add_space(28.0);
                        ui.label(RichText::new("History").color(BLUE));
                        ui.separator();
                        let history: Vec<_> = self
                            .messages
                            .iter()
                            .enumerate()
                            .filter(|(_, other)| other.email == m.email && other.folder != "Trash")
                            .map(|(i, other)| (i, other.subject.clone(), other.date.clone()))
                            .collect();
                        for (i, subject, date) in history {
                            ui.add_space(12.0);
                            ui.weak(date);
                            if ui
                                .selectable_label(self.selected == Some(i), subject)
                                .clicked()
                            {
                                self.selected = Some(i);
                                if !self.live {
                                    self.messages[i].unread = false;
                                }
                            }
                        }
                    } else {
                        ui.weak("Select a message to view its sender.");
                    }
                });
        }
        egui::SidePanel::left("messages")
            .default_width(310.0)
            .width_range(240.0..=420.0)
            .frame(
                egui::Frame::new()
                    .fill(Color32::from_rgb(36, 38, 35))
                    .inner_margin(10.0),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&self.folder).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak("Newest first");
                    });
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    let visible: Vec<_> = self
                        .messages
                        .iter()
                        .enumerate()
                        .filter(|(_, m)| self.matches(m))
                        .map(|(i, _)| i)
                        .collect();
                    if visible.is_empty() {
                        ui.weak("No messages here.");
                    }
                    for i in visible {
                        let m = &self.messages[i];
                        let response = egui::Frame::new()
                            .fill(if self.selected == Some(i) {
                                Color32::from_rgb(52, 60, 63)
                            } else {
                                Color32::TRANSPARENT
                            })
                            .inner_margin(6.0)
                            .show(ui, |ui| {
                                ui.set_min_width(ui.available_width());
                                ui.horizontal(|ui| {
                                    let (dot, _) = ui.allocate_exact_size(
                                        egui::vec2(8.0, 12.0),
                                        egui::Sense::hover(),
                                    );
                                    if m.unread {
                                        ui.painter().circle_filled(dot.center(), 3.0, BLUE);
                                    }
                                    ui.add(
                                        egui::Label::new(RichText::new(&m.sender).strong())
                                            .truncate(),
                                    );
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            let short_date =
                                                chrono::DateTime::parse_from_rfc2822(&m.date)
                                                    .map(|date| {
                                                        date.with_timezone(&chrono::Local)
                                                            .format("%b %e")
                                                            .to_string()
                                                    })
                                                    .unwrap_or_else(|_| m.date.clone());
                                            ui.small(short_date).on_hover_text(&m.date);
                                        },
                                    );
                                });
                                ui.add(
                                    egui::Label::new(if m.unread {
                                        RichText::new(&m.subject).strong()
                                    } else {
                                        RichText::new(&m.subject)
                                    })
                                    .truncate(),
                                );
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(m.body.replace('\n', " ")).color(MUTED),
                                    )
                                    .truncate(),
                                );
                            })
                            .response;
                        if ui
                            .interact(response.rect, ui.id().with(i), egui::Sense::click())
                            .clicked()
                        {
                            self.selected = Some(i);
                            if !self.live {
                                self.messages[i].unread = false;
                            }
                        }
                        ui.add_space(2.0);
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(READER).inner_margin(22.0))
            .show(ctx, |ui| {
                if let Some(m) = self.selected.and_then(|i| self.messages.get(i)).cloned() {
                    ui.heading(&m.subject);
                    ui.add_space(24.0);
                    ui.horizontal(|ui| {
                        avatar(ui, &m.sender, 40.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&m.sender).color(BLUE).strong());
                            ui.weak(format!("{} · {}", m.email, m.account));
                        });
                    });
                    ui.add_space(10.0);
                    ui.weak(format!(
                        "{}  •  {}{}",
                        m.date,
                        m.folder,
                        if m.flagged { "  •  Flagged" } else { "" }
                    ));
                    ui.add_space(12.0);
                    if (!m.attachments_loaded || !m.html_loaded) && ui
                            .add_enabled(
                                self.file_receiver.is_none(),
                                egui::Button::new("Load formatted message and attachments"),
                            )
                            .clicked()
                    {
                        let message = m.clone();
                        self.file_task(ctx, move || {
                            Ok(FileEvent::Content(Box::new(gmail::load_content(&message)?)))
                        });
                    }
                    if !m.attachments.is_empty() {
                        ui.weak("Downloads are saved to your Downloads folder (20 MiB per file).");
                    }
                    for file in &m.attachments {
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{} · {:.1} KiB",
                                file.name,
                                file.size as f64 / 1024.0
                            ));
                            if ui
                                .add_enabled(
                                    self.file_receiver.is_none()
                                        && file.size <= attachments::MAX_BYTES as u64,
                                    egui::Button::new("Save to Downloads"),
                                )
                                .clicked()
                            {
                                let (message, file) = (m.clone(), file.clone());
                                self.file_task(ctx, move || {
                                    Ok(FileEvent::Downloaded(gmail::download(&message, &file)?))
                                });
                            }
                        });
                    }
                    ui.add_space(16.0);
                    if m.html.is_some() {
                        ui.horizontal(|ui| {
                            ui.selectable_value(&mut self.formatted, true, "Formatted");
                            ui.selectable_value(&mut self.formatted, false, "Plain text");
                        });
                    }
                    if self.formatted && m.html.is_some() {
                        ui.weak("Images and active content are blocked. Links open only after you review the destination.");
                    }
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        if let Some(document) = m.html.as_ref().filter(|_| self.formatted) {
                            if let Some(url) = document.show(ui) { self.pending_link = Some(url); }
                        } else { ui.label(RichText::new(&m.body).size(15.0)); }
                    });
                } else {
                    ui.centered_and_justified(|ui| {
                        ui.weak("Select a message to read");
                    });
                }
            });
        if let Some(url) = self.pending_link.clone() {
            let mut open = true;
            egui::Window::new("Open email link")
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label("Destination:");
                    ui.add(egui::Label::new(&url).wrap().selectable(true));
                    ui.horizontal(|ui| {
                        if ui.button("Open in browser").clicked() {
                            if let Some(safe) = html::safe_link(&url) {
                                if let Err(error) = webbrowser::open(&safe) {
                                    self.status = format!("Could not open link: {error}");
                                }
                            }
                            self.pending_link = None;
                        }
                        if ui.button("Cancel").clicked() {
                            self.pending_link = None;
                        }
                    });
                });
            if !open {
                self.pending_link = None;
            }
        }
        let actions = self.settings.show(
            ctx,
            &self.accounts,
            &mut self.oauth_path,
            self.receiver.is_some(),
            self.cache_ready,
            &self.status,
        );
        if let Some((profile, password)) = actions.connect_profile {
            self.start_profile(ctx, profile, password);
        }
        if actions.connect_gmail {
            self.start(ctx, true);
        }
        if let Some(email) = actions.reconnect {
            self.start_account(ctx, false, Some(email));
        }
        let mut drafts_open = self.show_drafts;
        egui::Window::new("Saved drafts")
            .open(&mut drafts_open)
            .default_width(600.0)
            .show(ctx, |ui| {
                if ui
                    .add_enabled(
                        !self.sending && self.file_receiver.is_none(),
                        egui::Button::new("New draft"),
                    )
                    .clicked()
                {
                    self.draft(false);
                }
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for draft in self.saved_drafts.clone() {
                            ui.horizontal(|ui| {
                                ui.label(if draft.subject.is_empty() {
                                    "Untitled draft"
                                } else {
                                    &draft.subject
                                });
                                ui.weak(format!("{} → {}", draft.from, draft.to));
                                if ui
                                    .add_enabled(
                                        !self.sending && self.file_receiver.is_none(),
                                        egui::Button::new("Open"),
                                    )
                                    .clicked()
                                {
                                    self.open_saved_draft(&draft.id);
                                }
                                if ui
                                    .add_enabled(
                                        !self.sending && self.file_receiver.is_none(),
                                        egui::Button::new("Delete"),
                                    )
                                    .clicked()
                                {
                                    if draft.id == self.draft_state.id {
                                        self.discard_current();
                                    } else if let Err(e) = store::Store::open()
                                        .and_then(|s| s.delete_local_draft(&draft.id))
                                    {
                                        self.status = format!("Could not delete draft: {e}");
                                    }
                                    self.refresh_drafts();
                                }
                            });
                        }
                    });
                if self.saved_drafts.is_empty() {
                    ui.weak("No saved drafts");
                }
            });
        self.show_drafts = drafts_open;
        let mut open = self.compose;
        egui::Window::new("New message")
            .open(&mut open)
            .default_width(620.0)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!self.sending, |ui| {
                    egui::ComboBox::from_label("From")
                        .selected_text(&self.from)
                        .show_ui(ui, |ui| {
                            for account in &self.accounts {
                                ui.selectable_value(&mut self.from, account.clone(), account);
                            }
                        });
                    ui.label("To");
                    ui.text_edit_singleline(&mut self.to);
                    ui.label("Cc");
                    ui.text_edit_singleline(&mut self.cc);
                    ui.label("Subject");
                    ui.text_edit_singleline(&mut self.subject);
                    ui.add_space(12.0);
                    ui.add(
                        egui::TextEdit::multiline(&mut self.body)
                            .desired_rows(15)
                            .desired_width(f32::INFINITY),
                    );
                    if let Some(source) = self.draft_state.forward.clone() {
                        if !source.attachments.is_empty() {
                            ui.label("Original attachments — choose files to include:");
                            for (index, file) in source.attachments.iter().enumerate() {
                                let mut selected = self.forward_selected.contains(&index);
                                if ui
                                    .checkbox(
                                        &mut selected,
                                        format!(
                                            "{} · {:.1} KiB",
                                            file.name,
                                            file.size as f64 / 1024.0
                                        ),
                                    )
                                    .changed()
                                {
                                    if selected {
                                        self.forward_selected.insert(index);
                                    } else {
                                        self.forward_selected.remove(&index);
                                    }
                                }
                            }
                            if ui
                                .add_enabled(
                                    self.file_receiver.is_none()
                                        && !self.forward_selected.is_empty(),
                                    egui::Button::new("Include selected attachments"),
                                )
                                .clicked()
                            {
                                let selected =
                                    self.forward_selected.iter().copied().collect::<Vec<_>>();
                                let remaining = attachments::MAX_BYTES.saturating_sub(
                                    self.files.iter().map(|f| f.data.len()).sum::<usize>(),
                                );
                                self.file_task(ctx, move || {
                                    Ok(FileEvent::Forwarded(gmail::forward_files(
                                        &source, &selected, remaining,
                                    )?))
                                });
                            }
                        }
                    }
                    if ui
                        .add_enabled(
                            self.file_receiver.is_none(),
                            egui::Button::new("Attach file…"),
                        )
                        .clicked()
                    {
                        self.picker.start();
                    }
                    let mut remove = None;
                    for (index, file) in self.files.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.label(format!(
                                "{} · {:.1} KiB",
                                file.name,
                                file.data.len() as f64 / 1024.0
                            ));
                            if ui.button("Remove").clicked() {
                                remove = Some(index);
                            }
                        });
                    }
                    if let Some(index) = remove {
                        self.files.remove(index);
                        self.persist_attachments();
                    }
                    if self.file_receiver.is_some() {
                        ui.weak("Working on attachment…");
                    }
                    ui.weak(
                        "Attachments: up to 20 MiB total. Files are copied into the local draft.",
                    );
                    ui.weak(
                        "Draft saves locally. Send delivers through the selected Gmail account.",
                    );
                    if ui
                        .add_enabled(
                            self.receiver.is_none()
                                && self.file_receiver.is_none()
                                && !self.from.is_empty()
                                && !self.to.trim().is_empty(),
                            egui::Button::new("Send"),
                        )
                        .clicked()
                        && self.save_current(true)
                    {
                        let draft = self.record();
                        let files = self.files.clone();
                        self.worker(ctx, move |cancel| gmail::send(&draft, &files, cancel));
                        self.sending = true;
                    }
                    if ui
                        .add_enabled(
                            self.file_receiver.is_none(),
                            egui::Button::new("Discard draft"),
                        )
                        .clicked()
                    {
                        self.discard_current();
                    }
                });
            });
        self.compose = open && self.compose;
        if !self.sending && self.file_receiver.is_none() {
            if let Some(path) = self.picker.show(ctx) {
                self.import_attachment(ctx, path);
            }
        }
        if self.cache_ready
            && before
                != (
                    self.to.clone(),
                    self.subject.clone(),
                    self.body.clone(),
                    self.from.clone(),
                    self.cc.clone(),
                )
        {
            self.save_current(false);
        }
    }
}

fn main() -> eframe::Result {
    if let Some(result) = service::command_line() {
        if let Err(error) = result {
            eprintln!("{error:#}");
            std::process::exit(1);
        }
        return Ok(());
    }
    // Desktop launchers can inherit a stale Wayland socket after switching sessions.
    // Let winit use X11 when that socket is unavailable and DISPLAY is present.
    #[cfg(target_os = "linux")]
    if std::env::var_os("DISPLAY").is_some() {
        if let Some(display) = std::env::var_os("WAYLAND_DISPLAY") {
            let display = std::path::PathBuf::from(display);
            let socket = if display.is_absolute() {
                Some(display)
            } else {
                std::env::var_os("XDG_RUNTIME_DIR")
                    .map(|runtime| std::path::PathBuf::from(runtime).join(display))
            };
            if socket.is_none_or(|path| std::os::unix::net::UnixStream::connect(path).is_err()) {
                std::env::remove_var("WAYLAND_DISPLAY");
            }
        }
    }
    if let Err(error) = service::ensure_running() {
        eprintln!("Mail service unavailable: {error:#}");
    }
    eframe::run_native(
        "FocalMail",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1380.0, 820.0])
                .with_min_inner_size([900.0, 600.0]),
            ..Default::default()
        },
        Box::new(|cc| {
            html::install_fonts(&cc.egui_ctx);
            let mut visuals = egui::Visuals::dark();
            visuals.selection.bg_fill = Color32::from_rgb(48, 72, 85);
            visuals.selection.stroke.color = BLUE;
            visuals.panel_fill = PANEL;
            visuals.widgets.noninteractive.fg_stroke.color = TEXT;
            visuals.widgets.inactive.fg_stroke.color = TEXT;
            visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
            cc.egui_ctx.set_visuals(visuals);
            cc.egui_ctx.style_mut(|style| {
                style.spacing.item_spacing = egui::vec2(8.0, 3.0);
                style.spacing.button_padding = egui::vec2(9.0, 5.0);
                style
                    .text_styles
                    .insert(egui::TextStyle::Body, egui::FontId::proportional(13.0));
                style
                    .text_styles
                    .insert(egui::TextStyle::Button, egui::FontId::proportional(13.0));
                style
                    .text_styles
                    .insert(egui::TextStyle::Heading, egui::FontId::proportional(17.0));
            });
            Ok(Box::new(Mail::open()))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_starts_a_distinct_draft_without_carrying_old_attachments() {
        let mut mail = Mail {
            files: vec![attachments::DraftAttachment {
                name: "original.bin".into(),
                data: vec![42],
            }],
            ..Default::default()
        };
        let previous = mail.draft_state.id.clone();
        mail.draft(true);
        assert!(mail.compose);
        assert_ne!(mail.draft_state.id, previous);
        assert!(mail.files.is_empty());
    }

    #[test]
    fn moving_mail_removes_it_from_inbox_but_keeps_it_searchable_in_archive() {
        let mut mail = Mail::default();
        mail.messages[0].folder = "Archive".into();
        assert!(!mail.matches(&mail.messages[0]));
        mail.folder = "Archive".into();
        mail.query = "picnic".into();
        assert!(mail.matches(&mail.messages[0]));
        mail.query = "missing phrase".into();
        assert!(!mail.matches(&mail.messages[0]));
    }

    #[test]
    fn reply_addresses_selected_sender_without_sending() {
        let mut mail = Mail::default();
        mail.draft(true);
        assert!(mail.compose);
        assert_eq!(mail.to, "bob@example.com");
        assert_eq!(mail.subject, "Re: Long time no see!");
        assert!(mail.body.contains("> Hi Alice,"));
        assert_eq!(mail.messages.len(), 7);
    }

    #[test]
    fn composing_is_blocked_while_a_send_is_pending() {
        let mut mail = Mail {
            sending: true,
            body: "Pending body".into(),
            ..Default::default()
        };
        let id = mail.draft_state.id.clone();
        mail.draft(true);
        mail.draft(false);
        mail.discard_current();
        assert_eq!(mail.body, "Pending body");
        assert_eq!(mail.draft_state.id, id);
    }

    #[test]
    fn account_inbox_filters_out_other_accounts() {
        let mut mail = Mail {
            live: true,
            accounts: vec!["first@example.com".into(), "second@example.com".into()],
            ..Default::default()
        };
        mail.messages[0].account = mail.accounts[0].clone();
        mail.messages[1].account = mail.accounts[1].clone();
        mail.folder = mail.accounts[0].clone();
        assert!(mail.matches(&mail.messages[0]));
        assert!(!mail.matches(&mail.messages[1]));
    }
}
