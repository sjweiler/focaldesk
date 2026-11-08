use eframe::egui;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Gmail,
    Microsoft365,
    Manual,
}
impl Provider {
    pub fn label(self) -> &'static str {
        match self {
            Self::Gmail => "Google / Gmail",
            Self::Microsoft365 => "Microsoft 365 / Outlook",
            Self::Manual => "Other mail server",
        }
    }
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Incoming {
    #[default]
    Imap,
    Pop3,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Authentication {
    #[default]
    OAuth,
    Password,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Security {
    #[default]
    Tls,
    StartTls,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub email: String,
    pub provider: Provider,
    pub incoming: Incoming,
    pub authentication: Authentication,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: Security,
    #[serde(default = "enabled")]
    pub smtp_copy_to_sent: bool,
    pub username: String,
    pub client_id: String,
    pub tenant: String,
}
fn enabled() -> bool {
    true
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            email: String::new(),
            provider: Provider::Gmail,
            incoming: Incoming::Imap,
            authentication: Authentication::OAuth,
            host: String::new(),
            port: 993,
            security: Security::Tls,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_security: Security::StartTls,
            smtp_copy_to_sent: true,
            username: String::new(),
            client_id: String::new(),
            tenant: "common".into(),
        }
    }
}
impl Profile {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.email.contains('@') && !self.email.chars().any(char::is_whitespace),
            "Enter an email address"
        );
        if self.provider == Provider::Microsoft365 {
            anyhow::ensure!(
                !self.client_id.trim().is_empty(),
                "Enter a Microsoft application client ID"
            );
            anyhow::ensure!(
                !self.tenant.is_empty()
                    && self
                        .tenant
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
                "Invalid Microsoft tenant"
            );
        }
        if self.provider == Provider::Manual {
            anyhow::ensure!(
                !self.host.trim().is_empty() && !self.smtp_host.trim().is_empty(),
                "Enter incoming and outgoing server names"
            );
            anyhow::ensure!(
                self.port > 0 && self.smtp_port > 0,
                "Server ports must be greater than zero"
            );
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct Settings {
    pub open: bool,
    adding: bool,
    profile: Profile,
    profiles: Vec<Profile>,
    notice: String,
    account_states: serde_json::Value,
    password: String,
    last_status_check: Option<std::time::Instant>,
}

pub struct Actions {
    pub connect_gmail: bool,
    pub connect_profile: Option<(Profile, String)>,
    pub reconnect: Option<String>,
}
impl Settings {
    pub fn load() -> Self {
        match crate::store::Store::open().and_then(|s| s.profiles()) {
            Ok(profiles) => Self {
                profiles,
                ..Self::default()
            },
            Err(e) => Self {
                notice: format!("Could not load account settings: {e}"),
                ..Self::default()
            },
        }
    }
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        connected: &[String],
        oauth_path: &mut String,
        busy: bool,
        cache_ready: bool,
        status: &str,
    ) -> Actions {
        let mut actions = Actions {
            connect_gmail: false,
            connect_profile: None,
            reconnect: None,
        };
        if self.open
            && self
                .last_status_check
                .is_none_or(|time| time.elapsed().as_secs() >= 2)
        {
            self.account_states = crate::store::Store::open()
                .and_then(|s| s.account_status())
                .unwrap_or_default();
            self.last_status_check = Some(std::time::Instant::now());
        }
        let mut open = self.open;
        egui::Window::new("Settings").open(&mut open).default_width(650.0).default_height(540.0).resizable(true).show(ctx, |ui| {
            ui.heading("Notifications");
            if let Ok(store) = crate::store::Store::open() {
                let mut enabled = store.notifications_enabled().unwrap_or(true);
                if ui.checkbox(&mut enabled, "Notify about new unread mail").changed() {
                    if let Err(e) = store.set_notifications(enabled) { self.notice = e.to_string(); }
                }
            }
            ui.weak("Notifications show a message count. Initial mailbox imports are silent.");
            ui.separator();
            ui.heading("Accounts");
            ui.label("Add a provider account or configure incoming and outgoing mail servers.");
            ui.separator();
            for email in connected {
                ui.horizontal(|ui| {
                    ui.label(email);
                    let profile = crate::store::Store::open().and_then(|s| s.connected_profile(email)).ok().flatten();
                    ui.weak(profile.as_ref().map(|p| p.provider.label()).unwrap_or("Google / Gmail"));
                    if let Some(profile) = profile.as_ref() {
                        if ui.add_enabled(!busy, egui::Button::new("Edit")).clicked() { self.profile = profile.clone(); self.password.clear(); self.adding = true; }
                    }
                    if let Some(status) = self.account_states.as_array().and_then(|states| states.iter().find(|s| s["email"] == *email)) {
                        let state = status["state"].as_str().unwrap_or("offline");
                        ui.weak(state.replace('_', " ")).on_hover_text(status["detail"].as_str().unwrap_or(""));
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Reconnect")).clicked() { actions.reconnect = Some(email.clone()); }
                });
            }
            for profile in self.profiles.iter().filter(|p| !connected.contains(&p.email)) {
                ui.horizontal(|ui| {
                    ui.label(&profile.email); ui.weak(format!("{} · configuration only",profile.provider.label()));
                    if ui.button("Edit").clicked() { self.profile = profile.clone(); self.adding = true; }
                });
            }
            if connected.is_empty() && self.profiles.is_empty() { ui.weak("No accounts added yet."); }
            ui.add_space(10.0);
            if ui.button("+ Add account").clicked() { self.profile = Profile::default(); self.password.clear(); self.adding = true; self.notice.clear(); }
            if self.adding {
                ui.separator();
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    ui.heading("Add / configure account");
                    egui::ComboBox::from_label("Provider").selected_text(self.profile.provider.label()).show_ui(ui, |ui| {
                        for provider in [Provider::Gmail,Provider::Microsoft365,Provider::Manual] { ui.selectable_value(&mut self.profile.provider, provider, provider.label()); }
                    });
                    match self.profile.provider {
                        Provider::Gmail => {
                            ui.label("Mail access: Gmail REST API");
                            ui.label("Authentication: OAuth 2.0 — sign in through your browser");
                            ui.collapsing("Advanced · application OAuth configuration", |ui| {
                                ui.label("This development build needs a Google Desktop OAuth client JSON. Enable Gmail API access and add your address as a test user when using a Testing app.");
                                ui.hyperlink_to("Google Cloud OAuth clients", "https://console.cloud.google.com/auth/clients");
                                ui.label("Credentials JSON path");
                                ui.add(egui::TextEdit::singleline(oauth_path).desired_width(f32::INFINITY));
                            });
                            if oauth_path.trim().is_empty() { ui.weak("Set the credentials file in Advanced before signing in."); }
                            if ui.add_enabled(!busy && cache_ready && !oauth_path.trim().is_empty(),egui::Button::new("Sign in with Google")).clicked() { actions.connect_gmail = true; }
                            ui.weak("Sign-in requests permission to read, modify, and send Gmail. Reconnect older read-only accounts to enable actions.");
                            ui.weak("Mail is cached locally. Refresh credentials are stored in the desktop keyring.");
                        }
                        Provider::Microsoft365 => {
                            field(ui,"Email address",&mut self.profile.email);
                            ui.label("Mail access: Microsoft Graph REST API");
                            ui.label("Authentication: OAuth 2.0");
                            ui.collapsing("Advanced · Microsoft application", |ui| {
                                field(ui,"Application (client) ID",&mut self.profile.client_id);
                                field(ui,"Tenant ID or domain",&mut self.profile.tenant);
                            });
                            ui.label("Enable public client flows in your Microsoft app registration. Sign-in uses a browser device code and requests Mail.ReadWrite, Mail.Send and offline access.");
                            if ui.add_enabled(!busy && cache_ready, egui::Button::new("Sign in with Microsoft")).clicked() {
                                match self.profile.validate() { Ok(()) => actions.connect_profile = Some((self.profile.clone(), String::new())), Err(e) => self.notice = e.to_string() }
                            }
                            self.save_button(ui, cache_ready);
                        }
                        Provider::Manual => {
                            field(ui,"Email address",&mut self.profile.email);
                            field(ui,"Username",&mut self.profile.username);
                            egui::ComboBox::from_label("Incoming protocol").selected_text(if self.profile.incoming == Incoming::Imap {"IMAP"} else {"POP3"}).show_ui(ui, |ui| {
                                if ui.selectable_value(&mut self.profile.incoming,Incoming::Imap,"IMAP").changed() { self.profile.port = if self.profile.security == Security::Tls {993} else {143}; }
                                if ui.selectable_value(&mut self.profile.incoming,Incoming::Pop3,"POP3").changed() { self.profile.port = if self.profile.security == Security::Tls {995} else {110}; }
                            });
                            field(ui,"Incoming server",&mut self.profile.host);
                            server(ui,"incoming",&mut self.profile.port,&mut self.profile.security);
                            ui.add_space(8.0); ui.label("Outgoing protocol: SMTP");
                            field(ui,"Outgoing server",&mut self.profile.smtp_host);
                            server(ui,"outgoing",&mut self.profile.smtp_port,&mut self.profile.smtp_security);
                            ui.checkbox(&mut self.profile.smtp_copy_to_sent, "Save a copy in the server Sent folder").on_hover_text("Disable if your SMTP server saves sent messages automatically.");
                            egui::ComboBox::from_label("Authentication").selected_text(if self.profile.authentication == Authentication::OAuth {"OAuth 2.0"} else {"Password / app password"}).show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.profile.authentication,Authentication::OAuth,"OAuth 2.0");
                                ui.selectable_value(&mut self.profile.authentication,Authentication::Password,"Password / app password");
                            });
                            let supported = self.profile.incoming == Incoming::Imap && self.profile.authentication == Authentication::Password;
                            if supported {
                                ui.label("Password / app password");
                                ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                                ui.weak("TLS is required. The password is stored in the desktop keyring after both server connections succeed.");
                                if ui.add_enabled(!busy && cache_ready && !self.password.is_empty(), egui::Button::new("Connect IMAP and SMTP")).clicked() {
                                    match self.profile.validate() { Ok(()) => actions.connect_profile = Some((self.profile.clone(), std::mem::take(&mut self.password))), Err(e) => self.notice = e.to_string() }
                                }
                            } else { ui.label("POP3 and generic IMAP OAuth are configuration-only. Choose IMAP and Password / app password to connect."); }
                            self.save_button(ui, cache_ready);
                        }
                    }
                });
            }
            if !self.notice.is_empty() { ui.separator(); ui.label(&self.notice); }
            if !status.is_empty() { ui.separator(); if busy { ui.spinner(); } ui.label(status); }
        });
        self.open = open;
        actions
    }
    fn save_button(&mut self, ui: &mut egui::Ui, cache_ready: bool) {
        if ui
            .add_enabled(cache_ready, egui::Button::new("Save configuration"))
            .clicked()
        {
            match self
                .profile
                .validate()
                .and_then(|()| crate::store::Store::open()?.save_profile(&self.profile))
            {
                Ok(()) => {
                    self.profiles.retain(|p| {
                        !(p.email == self.profile.email && p.provider == self.profile.provider)
                    });
                    self.profiles.push(self.profile.clone());
                    self.notice = "Configuration saved. This account is not connected.".into();
                }
                Err(e) => self.notice = e.to_string(),
            }
        }
    }
}
fn field(ui: &mut egui::Ui, name: &str, value: &mut String) {
    ui.label(name);
    ui.add(egui::TextEdit::singleline(value).desired_width(f32::INFINITY));
}
fn server(ui: &mut egui::Ui, id: &str, port: &mut u16, security: &mut Security) {
    ui.horizontal(|ui| {
        ui.label("Port");
        ui.add(egui::DragValue::new(port).range(1..=65535));
        egui::ComboBox::from_id_salt(id)
            .selected_text(if *security == Security::Tls {
                "SSL/TLS"
            } else {
                "STARTTLS"
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(security, Security::Tls, "SSL/TLS");
                ui.selectable_value(security, Security::StartTls, "STARTTLS");
            });
    });
}
