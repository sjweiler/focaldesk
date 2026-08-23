use focaldesk_ipc::{DialogIpcRequest, DialogIpcResponse, send_dialog_request};
use focaldesk_permissions::identity::{AppIdentity, AppMetadata};
use focaldesk_permissions::manager::PermissionManager;
use focaldesk_permissions::policy::DefaultPolicy;
use focaldesk_permissions::prompt::{PermissionPrompter, UserPromptResponse};
use focaldesk_permissions::request::{PermissionRequest, PermissionResource, PermissionTarget};
use focaldesk_permissions::store::PermissionStore;
use focaldesk_permissions::types::{PermissionDecision, PermissionScope, PermissionState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiPermissionMode {
    Prompt,
    AllowSession,
    AllowPersistent,
    Deny,
}

impl AiPermissionMode {
    fn from_env() -> Self {
        match std::env::var("FOCALDESK_AI_PERMISSION")
            .unwrap_or_else(|_| "prompt".to_string())
            .to_lowercase()
            .as_str()
        {
            "prompt" => Self::Prompt,
            "allow" | "allow-session" => Self::AllowSession,
            "allow-persistent" | "persist" | "persistent" => Self::AllowPersistent,
            "deny" | "deny-session" => Self::Deny,
            other => {
                tracing::warn!(
                    target: "focaldesk.ai",
                    permission_mode = other,
                    "unknown FOCALDESK_AI_PERMISSION value; falling back to prompt"
                );
                Self::Prompt
            }
        }
    }
}

#[derive(Debug, Default, Clone)]
struct PersistentPermissionStore {
    path: PathBuf,
    entries: HashMap<PermissionKey, PermissionState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PermissionKey {
    app_identity: String,
    resource: String,
    target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedPermissionEntry {
    app_identity: String,
    resource: String,
    target: String,
    decision: String,
    scope: String,
    updated_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistedPermissionFile {
    #[serde(default)]
    entries: Vec<PersistedPermissionEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiPermissionRecord {
    pub app_identity: String,
    pub resource: PermissionResource,
    pub target: PermissionTarget,
    pub decision: PermissionDecision,
    pub scope: PermissionScope,
    pub updated_at: std::time::SystemTime,
}

impl PersistentPermissionStore {
    fn load_default() -> Self {
        Self::load_from_path(permission_store_path())
    }

    fn load_from_path(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            entries: HashMap::new(),
        };

        match fs::read_to_string(&store.path) {
            Ok(text) => match toml::from_str::<PersistedPermissionFile>(&text) {
                Ok(file) => {
                    for entry in file.entries {
                        if let Some(state) = persisted_entry_to_state(&entry) {
                            store.entries.insert(
                                PermissionKey {
                                    app_identity: entry.app_identity,
                                    resource: entry.resource,
                                    target: entry.target,
                                },
                                state,
                            );
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        target: "focaldesk.ai",
                        path = %store.path.display(),
                        error = %err,
                        "failed to parse AI permission store; starting empty"
                    );
                }
            },
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => {
                tracing::warn!(
                    target: "focaldesk.ai",
                    path = %store.path.display(),
                    error = %err,
                    "failed to read AI permission store; starting empty"
                );
            }
        }

        store
    }

    fn save(&self) -> Result<(), focaldesk_permissions::error::PermissionError> {
        let file = PersistedPermissionFile {
            entries: self
                .entries
                .iter()
                .map(|(key, state)| state_to_persisted_entry(key, state))
                .collect(),
        };

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                focaldesk_permissions::error::PermissionError::Store(err.to_string())
            })?;
            if parent.file_name().is_some_and(|name| name == "focaldesk") {
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|err| {
                    focaldesk_permissions::error::PermissionError::Store(err.to_string())
                })?;
            }
        }

        let text = toml::to_string_pretty(&file)
            .map_err(|err| focaldesk_permissions::error::PermissionError::Store(err.to_string()))?;
        write_private_atomic(&self.path, text.as_bytes())
            .map_err(|err| focaldesk_permissions::error::PermissionError::Store(err.to_string()))
    }

    fn records(&self) -> Vec<AiPermissionRecord> {
        self.entries
            .iter()
            .map(|(key, state)| AiPermissionRecord {
                app_identity: key.app_identity.clone(),
                resource: state.resource,
                target: state.target.clone(),
                decision: state.decision,
                scope: state.scope,
                updated_at: state.updated_at,
            })
            .collect()
    }

    fn revoke_record(
        &mut self,
        record: &AiPermissionRecord,
    ) -> Result<(), focaldesk_permissions::error::PermissionError> {
        let key = PermissionKey {
            app_identity: record.app_identity.clone(),
            resource: permission_resource_key(record.resource),
            target: permission_target_key(&record.target),
        };
        self.entries.remove(&key);
        self.save()
    }
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "AI permission path has no parent",
        )
    })?;
    let nonce = rand::random::<u64>();
    let temp = parent.join(format!(
        ".ai-permissions-{}-{nonce:016x}.tmp",
        std::process::id()
    ));
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

impl PermissionStore for PersistentPermissionStore {
    fn get(
        &self,
        app: &AppIdentity,
        resource: PermissionResource,
        target: &PermissionTarget,
    ) -> Option<PermissionState> {
        let key = PermissionKey {
            app_identity: app_identity_key(app),
            resource: permission_resource_key(resource),
            target: permission_target_key(target),
        };

        self.entries.get(&key).cloned().map(|mut state| {
            state.app = app.clone();
            state
        })
    }

    fn set(
        &mut self,
        state: PermissionState,
    ) -> Result<(), focaldesk_permissions::error::PermissionError> {
        let key = PermissionKey {
            app_identity: app_identity_key(&state.app),
            resource: permission_resource_key(state.resource),
            target: permission_target_key(&state.target),
        };
        self.entries.insert(key, state);
        self.save()
    }

    fn list_for_app(&self, app: &AppIdentity) -> Vec<PermissionState> {
        let app_key = app_identity_key(app);
        self.entries
            .iter()
            .filter(|(key, _)| key.app_identity == app_key)
            .map(|(_, state)| {
                let mut state = state.clone();
                state.app = app.clone();
                state
            })
            .collect()
    }

    fn revoke(
        &mut self,
        app: &AppIdentity,
        resource: PermissionResource,
        target: &PermissionTarget,
    ) -> Result<(), focaldesk_permissions::error::PermissionError> {
        let key = PermissionKey {
            app_identity: app_identity_key(app),
            resource: permission_resource_key(resource),
            target: permission_target_key(target),
        };
        self.entries.remove(&key);
        self.save()
    }
}

fn permission_store_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("focaldesk")
        .join("ai_permissions.toml")
}

fn app_identity_key(app: &AppIdentity) -> String {
    match app {
        AppIdentity::DesktopId(id)
        | AppIdentity::FlatpakId(id)
        | AppIdentity::WaylandAppId(id)
        | AppIdentity::ExecutablePath(id) => id.clone(),
        AppIdentity::Unknown => "unknown".to_string(),
    }
}

fn permission_resource_key(resource: PermissionResource) -> String {
    format!("{resource:?}")
}

fn permission_target_key(target: &PermissionTarget) -> String {
    match target {
        PermissionTarget::Global => "global".to_string(),
        PermissionTarget::Named(name) => name.clone(),
    }
}

pub fn list_ai_permission_records() -> anyhow::Result<Vec<AiPermissionRecord>> {
    Ok(PersistentPermissionStore::load_default().records())
}

pub fn revoke_ai_permission(record: &AiPermissionRecord) -> anyhow::Result<()> {
    let mut store = PersistentPermissionStore::load_default();
    store
        .revoke_record(record)
        .map_err(|err| anyhow::anyhow!("failed to revoke AI permission: {err:?}"))
}

fn state_to_persisted_entry(
    key: &PermissionKey,
    state: &PermissionState,
) -> PersistedPermissionEntry {
    PersistedPermissionEntry {
        app_identity: key.app_identity.clone(),
        resource: key.resource.clone(),
        target: key.target.clone(),
        decision: format!("{:?}", state.decision),
        scope: format!("{:?}", state.scope),
        updated_at_unix: state
            .updated_at
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    }
}

fn persisted_entry_to_state(entry: &PersistedPermissionEntry) -> Option<PermissionState> {
    let resource = match entry.resource.as_str() {
        "AiChat" => PermissionResource::AiChat,
        "Screenshot" => PermissionResource::Screenshot,
        "Screencast" => PermissionResource::Screencast,
        "ScreenShareWindow" => PermissionResource::ScreenShareWindow,
        "ScreenShareOutput" => PermissionResource::ScreenShareOutput,
        "Microphone" => PermissionResource::Microphone,
        "Camera" => PermissionResource::Camera,
        "ClipboardRead" => PermissionResource::ClipboardRead,
        "ClipboardWrite" => PermissionResource::ClipboardWrite,
        "RemoteInput" => PermissionResource::RemoteInput,
        "Notifications" => PermissionResource::Notifications,
        "FileOpen" => PermissionResource::FileOpen,
        "FileSave" => PermissionResource::FileSave,
        _ => return None,
    };

    let target = if entry.target == "global" {
        PermissionTarget::Global
    } else {
        PermissionTarget::Named(entry.target.clone())
    };

    let decision = match entry.decision.as_str() {
        "Allow" => PermissionDecision::Allow,
        "Deny" => PermissionDecision::Deny,
        "Ask" => PermissionDecision::Ask,
        _ => return None,
    };

    let scope = match entry.scope.as_str() {
        "Once" => PermissionScope::Once,
        "Session" => PermissionScope::Session,
        "Persistent" => PermissionScope::Persistent,
        _ => return None,
    };

    Some(PermissionState {
        app: AppIdentity::Unknown,
        resource,
        target,
        decision,
        scope,
        updated_at: std::time::UNIX_EPOCH
            .checked_add(std::time::Duration::from_secs(entry.updated_at_unix))
            .unwrap_or(std::time::UNIX_EPOCH),
    })
}

#[derive(Debug, Clone)]
struct AiPermissionPrompter {
    mode: AiPermissionMode,
    prompt_title: String,
    prompt_message: String,
    allow_persistent: bool,
}

impl AiPermissionPrompter {
    fn new() -> Self {
        Self {
            mode: AiPermissionMode::from_env(),
            prompt_title: "Allow AI chat?".to_string(),
            prompt_message: "An AI request wants to use the current prompt.".to_string(),
            allow_persistent: true,
        }
    }

    fn set_prompt_context(
        &mut self,
        prompt_title: impl Into<String>,
        prompt_message: impl Into<String>,
        allow_persistent: bool,
    ) {
        self.prompt_title = prompt_title.into();
        self.prompt_message = prompt_message.into();
        self.allow_persistent = allow_persistent;
    }
}

impl PermissionPrompter for AiPermissionPrompter {
    fn prompt(&mut self, request: &PermissionRequest) -> UserPromptResponse {
        if matches!(self.mode, AiPermissionMode::Prompt) {
            if let Some(response) = prompt_from_desktop_or_terminal(
                request,
                &self.prompt_title,
                &self.prompt_message,
                self.allow_persistent,
            ) {
                return response;
            }
        }

        let decision = match self.mode {
            AiPermissionMode::AllowSession | AiPermissionMode::AllowPersistent => {
                PermissionDecision::Allow
            }
            AiPermissionMode::Deny | AiPermissionMode::Prompt => PermissionDecision::Deny,
        };

        let scope = match self.mode {
            AiPermissionMode::AllowPersistent => PermissionScope::Persistent,
            _ => PermissionScope::Session,
        };

        tracing::info!(
            target: "focaldesk.ai",
            app = ?request.app.identity,
            resource = ?request.resource,
            target = ?request.target,
            decision = ?decision,
            scope = ?scope,
            "AI permission prompt resolved"
        );

        UserPromptResponse { decision, scope }
    }
}

fn prompt_from_desktop_or_terminal(
    request: &PermissionRequest,
    title: &str,
    message: &str,
    allow_persistent: bool,
) -> Option<UserPromptResponse> {
    if let Some(response) = prompt_from_desktop(title, message, allow_persistent) {
        return Some(response);
    }

    prompt_from_terminal(request, title, message, allow_persistent)
}

fn prompt_from_desktop(
    title: &str,
    message: &str,
    allow_persistent: bool,
) -> Option<UserPromptResponse> {
    let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
    let response = send_dialog_request(&DialogIpcRequest::AiPermissionPrompt {
        request_id,
        title: title.to_string(),
        message: message.to_string(),
        allow_persistent,
    });

    desktop_response_to_user_prompt(request_id, response)
}

fn desktop_response_to_user_prompt(
    request_id: u64,
    response: Result<DialogIpcResponse, String>,
) -> Option<UserPromptResponse> {
    match response {
        Ok(DialogIpcResponse::AiPermissionDecision {
            request_id: response_id,
            allow,
            persistent,
        }) if response_id == request_id => {
            let response = if allow {
                UserPromptResponse {
                    decision: PermissionDecision::Allow,
                    scope: if persistent {
                        PermissionScope::Persistent
                    } else {
                        PermissionScope::Session
                    },
                }
            } else {
                UserPromptResponse {
                    decision: PermissionDecision::Deny,
                    scope: PermissionScope::Session,
                }
            };

            tracing::info!(
                target: "focaldesk.ai",
                request_id = response_id,
                decision = ?response.decision,
                scope = ?response.scope,
                "AI permission prompt answered by dialog broker"
            );

            Some(response)
        }
        Ok(other) => {
            tracing::warn!(
                target: "focaldesk.ai",
                response = ?other,
                "unexpected AI permission response from dialog broker"
            );
            Some(UserPromptResponse {
                decision: PermissionDecision::Deny,
                scope: PermissionScope::Session,
            })
        }
        Err(err) => {
            tracing::debug!(
                target: "focaldesk.ai",
                error = %err,
                "AI dialog broker prompt unavailable"
            );
            None
        }
    }
}

fn prompt_from_terminal(
    request: &PermissionRequest,
    title: &str,
    message: &str,
    allow_persistent: bool,
) -> Option<UserPromptResponse> {
    if !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        tracing::warn!(
            target: "focaldesk.ai",
            "AI permission prompt requested but no interactive terminal is available"
        );
        return None;
    }

    let resource = format!("{:?}", request.resource);
    let target = match &request.target {
        PermissionTarget::Global => "global".to_string(),
        PermissionTarget::Named(name) => name.clone(),
    };

    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "\n{title}");
    let _ = writeln!(stderr, "{message}");
    let _ = writeln!(stderr, "Request: {resource}");
    let _ = writeln!(stderr, "Target: {target}");
    if allow_persistent {
        let _ = write!(stderr, "Allow? [y]es / [p]ersistent / [n]o: ");
    } else {
        let _ = write!(stderr, "Allow? [y]es / [n]o: ");
    }
    let _ = stderr.flush();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return None;
    }

    let response = match input.trim().to_lowercase().as_str() {
        "y" | "yes" | "allow" => UserPromptResponse {
            decision: PermissionDecision::Allow,
            scope: PermissionScope::Session,
        },
        "p" | "persist" | "persistent" | "remember" if allow_persistent => UserPromptResponse {
            decision: PermissionDecision::Allow,
            scope: PermissionScope::Persistent,
        },
        _ => UserPromptResponse {
            decision: PermissionDecision::Deny,
            scope: PermissionScope::Session,
        },
    };

    tracing::info!(
        target: "focaldesk.ai",
        app = ?request.app.identity,
        resource = ?request.resource,
        target = ?request.target,
        decision = ?response.decision,
        scope = ?response.scope,
        "AI permission prompt answered from terminal"
    );

    Some(response)
}

static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

struct AiPermissionGate {
    manager:
        Mutex<PermissionManager<PersistentPermissionStore, DefaultPolicy, AiPermissionPrompter>>,
}

impl AiPermissionGate {
    fn new() -> Self {
        let prompter = AiPermissionPrompter::new();
        let store = PersistentPermissionStore::load_default();
        tracing::info!(
            target: "focaldesk.ai",
            permission_mode = ?prompter.mode,
            permission_store = %permission_store_path().display(),
            "AI permission gate initialized"
        );

        Self {
            manager: Mutex::new(PermissionManager {
                store,
                policy: DefaultPolicy,
                prompter,
                active_grants: Vec::new(),
            }),
        }
    }

    fn authorize_chat(
        &self,
        prompt_title: &str,
        prompt_message: &str,
        allow_persistent: bool,
    ) -> anyhow::Result<()> {
        let request = PermissionRequest {
            app: AppMetadata {
                identity: app_identity(),
                pid: Some(std::process::id()),
                window_title: Some("focaldesk AI service".into()),
                sandboxed: false,
            },
            resource: PermissionResource::AiChat,
            target: PermissionTarget::Global,
        };

        let mut manager = self
            .manager
            .lock()
            .map_err(|_| anyhow::anyhow!("AI permission gate mutex poisoned"))?;
        manager
            .prompter
            .set_prompt_context(prompt_title, prompt_message, allow_persistent);
        let result = manager
            .authorize(request)
            .map_err(|err| anyhow::anyhow!("AI permission check failed: {err:?}"))?;

        if let Some(token) = result {
            tracing::info!(
                target: "focaldesk.ai",
                grant_token = %token.0,
                "AI chat permission granted"
            );
            Ok(())
        } else {
            tracing::warn!(target: "focaldesk.ai", "AI chat permission denied");
            Err(anyhow::anyhow!(
                "AI chat is not allowed by the current permission policy"
            ))
        }
    }
}

fn app_identity() -> AppIdentity {
    std::env::current_exe()
        .ok()
        .and_then(|path| executable_name(path))
        .map(AppIdentity::ExecutablePath)
        .unwrap_or(AppIdentity::Unknown)
}

fn executable_name(path: PathBuf) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
}

pub(crate) fn authorize_ai_chat(
    prompt_title: &str,
    prompt_message: &str,
    allow_persistent: bool,
) -> anyhow::Result<()> {
    static GATE: std::sync::OnceLock<AiPermissionGate> = std::sync::OnceLock::new();
    GATE.get_or_init(AiPermissionGate::new).authorize_chat(
        prompt_title,
        prompt_message,
        allow_persistent,
    )
}

/// Require a fresh, one-shot approval for an exact model-proposed desktop
/// mutation or destructive AI-data operation. This deliberately bypasses
/// saved AI-chat grants and environment allow modes: prior consent to use a
/// model is not consent to change desktop state or delete stored data.
pub(crate) fn confirm_ai_action(tool: &str, title: &str, message: &str) -> anyhow::Result<()> {
    let resource = match tool {
        "focus_window" | "move_window_to_workspace" => PermissionResource::RemoteInput,
        "show_notification" => PermissionResource::Notifications,
        "open_settings_panel" | "forget_memory" | "clear_memory" => PermissionResource::AiChat,
        _ => {
            return Err(anyhow::anyhow!(
                "AI action is not eligible for confirmation: {tool}"
            ));
        }
    };
    let request = PermissionRequest {
        app: AppMetadata {
            identity: app_identity(),
            pid: Some(std::process::id()),
            window_title: Some("focaldesk AI action".into()),
            sandboxed: false,
        },
        resource,
        target: PermissionTarget::Named(tool.to_string()),
    };
    let response = prompt_from_desktop_or_terminal(&request, title, message, false)
        .ok_or_else(|| anyhow::anyhow!("native AI action confirmation is unavailable"))?;
    tracing::info!(
        target: "focaldesk.ai",
        tool,
        decision = ?response.decision,
        "one-shot AI action confirmation resolved"
    );
    if response.decision == PermissionDecision::Allow {
        Ok(())
    } else {
        Err(anyhow::anyhow!("AI action was denied by the user"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use focaldesk_permissions::prompt::PermissionPrompter;
    use focaldesk_permissions::prompt::UserPromptResponse;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_permission_store_path(test_name: &str) -> PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        PathBuf::from("/tmp").join(format!(
            "focaldesk-ai-permissions-{test_name}-{}-{stamp}.toml",
            std::process::id()
        ))
    }

    #[derive(Debug, Clone)]
    struct ScriptedPrompter {
        response: UserPromptResponse,
    }

    impl PermissionPrompter for ScriptedPrompter {
        fn prompt(&mut self, _request: &PermissionRequest) -> UserPromptResponse {
            self.response.clone()
        }
    }

    #[test]
    fn persistent_permission_round_trips_through_disk() {
        let path = temp_permission_store_path("roundtrip");

        let mut store = PersistentPermissionStore::load_from_path(path.clone());
        let state = PermissionState {
            app: AppIdentity::ExecutablePath("focaldesk-ai".to_string()),
            resource: PermissionResource::AiChat,
            target: PermissionTarget::Global,
            decision: PermissionDecision::Allow,
            scope: PermissionScope::Persistent,
            updated_at: UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        };

        store.set(state.clone()).expect("store should save");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );

        let reloaded = PersistentPermissionStore::load_from_path(path.clone());
        let loaded = reloaded
            .get(
                &AppIdentity::ExecutablePath("focaldesk-ai".to_string()),
                PermissionResource::AiChat,
                &PermissionTarget::Global,
            )
            .expect("stored permission should reload");

        assert_eq!(loaded.app, state.app);
        assert_eq!(loaded.resource, state.resource);
        assert_eq!(loaded.target, state.target);
        assert_eq!(loaded.decision, state.decision);
        assert_eq!(loaded.scope, state.scope);
        assert_eq!(loaded.updated_at, state.updated_at);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn persistent_permission_revoke_removes_saved_entry() {
        let path = temp_permission_store_path("revoke");

        let mut store = PersistentPermissionStore::load_from_path(path.clone());
        let state = PermissionState {
            app: AppIdentity::ExecutablePath("focaldesk-ai".to_string()),
            resource: PermissionResource::AiChat,
            target: PermissionTarget::Global,
            decision: PermissionDecision::Allow,
            scope: PermissionScope::Persistent,
            updated_at: UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000),
        };

        store.set(state.clone()).expect("store should save");

        let record = AiPermissionRecord {
            app_identity: "focaldesk-ai".to_string(),
            resource: PermissionResource::AiChat,
            target: PermissionTarget::Global,
            decision: PermissionDecision::Allow,
            scope: PermissionScope::Persistent,
            updated_at: state.updated_at,
        };

        store
            .revoke_record(&record)
            .expect("record should revoke cleanly");

        let reloaded = PersistentPermissionStore::load_from_path(path.clone());
        assert!(
            reloaded
                .get(
                    &AppIdentity::ExecutablePath("focaldesk-ai".to_string()),
                    PermissionResource::AiChat,
                    &PermissionTarget::Global,
                )
                .is_none(),
            "revoked permission should not reload"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn desktop_allow_persistent_response_maps_to_persistent_prompt_response() {
        let response = desktop_response_to_user_prompt(
            7,
            Ok(DialogIpcResponse::AiPermissionDecision {
                request_id: 7,
                allow: true,
                persistent: true,
            }),
        )
        .expect("desktop response should map");

        assert_eq!(response.decision, PermissionDecision::Allow);
        assert_eq!(response.scope, PermissionScope::Persistent);
    }

    #[test]
    fn permission_manager_persists_allow_from_prompt_response() {
        let path = temp_permission_store_path("manager");

        let response = UserPromptResponse {
            decision: PermissionDecision::Allow,
            scope: PermissionScope::Persistent,
        };
        let prompter = ScriptedPrompter { response };
        let mut manager = PermissionManager {
            store: PersistentPermissionStore::load_from_path(path.clone()),
            policy: DefaultPolicy,
            prompter,
            active_grants: Vec::new(),
        };
        let request = PermissionRequest {
            app: AppMetadata {
                identity: app_identity(),
                pid: Some(std::process::id()),
                window_title: Some("focaldesk AI service".into()),
                sandboxed: false,
            },
            resource: PermissionResource::AiChat,
            target: PermissionTarget::Global,
        };

        let token = manager
            .authorize(request)
            .expect("authorization should succeed");
        assert!(token.is_some(), "persistent allow should grant access");

        let reloaded = PersistentPermissionStore::load_from_path(path.clone());
        let stored = reloaded
            .get(
                &app_identity(),
                PermissionResource::AiChat,
                &PermissionTarget::Global,
            )
            .expect("persistent AI permission should be saved");

        assert_eq!(stored.decision, PermissionDecision::Allow);
        assert_eq!(stored.scope, PermissionScope::Persistent);

        let _ = fs::remove_file(path);
    }
}
