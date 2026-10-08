use anyhow::{Context, Result, anyhow, bail};
use hmac::{Hmac, Mac};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

pub const CONNECTOR_MANIFEST_VERSION: u16 = 1;
const TRUST_STORE_VERSION: u16 = 1;
const MAX_TRUST_STORE_BYTES: u64 = 256 * 1024;
const SIGNATURE_WINDOW_SECONDS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorRuntimeManifest {
    pub executable: String,
    #[serde(default)]
    pub args: Vec<String>,
    pub poll_interval_seconds: u64,
    pub memory_max_mib: u32,
    pub cpu_quota_percent: u8,
}

impl ConnectorRuntimeManifest {
    fn validate(&self) -> Result<()> {
        let path = Path::new(&self.executable);
        if !path.is_absolute()
            || self.executable.len() > 500
            || self.args.len() > 32
            || self.args.iter().any(|arg| arg.len() > 500)
            || !(5..=86_400).contains(&self.poll_interval_seconds)
            || !(16..=512).contains(&self.memory_max_mib)
            || !(1..=100).contains(&self.cpu_quota_percent)
        {
            bail!("connector runtime declaration is out of bounds");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorManifest {
    pub manifest_version: u16,
    pub id: String,
    pub name: String,
    pub description: String,
    pub version: String,
    pub event_sources: Vec<crate::EventSource>,
    pub event_fields: BTreeMap<crate::EventSource, Vec<String>>,
    #[serde(default)]
    pub network_domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime: Option<ConnectorRuntimeManifest>,
    #[serde(default)]
    pub built_in: bool,
}

impl ConnectorManifest {
    pub fn validate(&mut self) -> Result<()> {
        if self.manifest_version != CONNECTOR_MANIFEST_VERSION {
            bail!("unsupported connector manifest version");
        }
        validate_id(&self.id, "connector")?;
        if self.name.trim().is_empty()
            || self.name.chars().count() > 100
            || self.description.chars().count() > 500
            || self.version.trim().is_empty()
            || self.version.chars().count() > 40
            || self.event_sources.is_empty()
            || self.event_sources.len() > 5
            || self.network_domains.len() > 16
        {
            bail!("connector manifest is out of bounds");
        }
        self.event_sources.sort();
        self.event_sources.dedup();
        for source in &self.event_sources {
            let fields = self
                .event_fields
                .get_mut(source)
                .ok_or_else(|| anyhow!("connector source has no field schema"))?;
            fields.sort();
            fields.dedup();
            let supported = crate::supported_event_fields(*source);
            if fields.is_empty()
                || fields
                    .iter()
                    .any(|field| !supported.contains(&field.as_str()))
            {
                bail!("connector schema contains unsupported fields");
            }
        }
        if self
            .event_fields
            .keys()
            .any(|source| !self.event_sources.contains(source))
        {
            bail!("connector schema declares an undeclared source");
        }
        for domain in &self.network_domains {
            if domain.is_empty()
                || domain.len() > 253
                || domain.contains('/')
                || domain.contains(':')
                || !domain
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
            {
                bail!("connector network domain is invalid");
            }
        }
        if let Some(handle) = &self.signing_key_handle {
            let required_prefix = format!("connectors/{}/", self.id);
            if handle.len() > 200
                || !handle.starts_with(&required_prefix)
                || handle.len() == required_prefix.len()
                || !handle.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.')
                })
            {
                bail!("connector signing-key handle is invalid or outside its namespace");
            }
        }
        if !self.built_in && self.signing_key_handle.is_none() {
            bail!("external connectors require an opaque signing-key handle");
        }
        if let Some(runtime) = &self.runtime {
            runtime.validate()?;
        }
        if self.built_in && self.runtime.is_some() {
            bail!("built-in connectors use the managed in-process adapters");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorHealth {
    Disabled,
    Ready,
    Healthy,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConnectorStatus {
    pub manifest: ConnectorManifest,
    pub enabled: bool,
    pub network_allowed: bool,
    pub health: ConnectorHealth,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_at_unix: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    pub rollback_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConnectorEventRequest {
    pub connector_id: String,
    pub source: crate::EventSource,
    pub timestamp_unix: u64,
    pub nonce: String,
    pub payload: Value,
    pub signature_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConnectorRecord {
    manifest: ConnectorManifest,
    enabled: bool,
    network_allowed: bool,
    health: ConnectorHealth,
    #[serde(default)]
    last_event_at_unix: Option<u64>,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    rollback: Option<ConnectorManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TrustStoreDocument {
    version: u16,
    connectors: BTreeMap<String, ConnectorRecord>,
    #[serde(default)]
    source_policies: BTreeMap<crate::EventSource, crate::EventSourcePolicy>,
}

struct ConnectorRegistryState {
    records: BTreeMap<String, ConnectorRecord>,
    source_policies: BTreeMap<crate::EventSource, crate::EventSourcePolicy>,
    recent_nonces: BTreeMap<(String, String), u64>,
}

pub struct ConnectorRegistry {
    state: Mutex<ConnectorRegistryState>,
    store_path: Option<PathBuf>,
}

impl Default for ConnectorRegistry {
    fn default() -> Self {
        Self::memory()
    }
}

impl ConnectorRegistry {
    pub fn memory() -> Self {
        Self {
            state: Mutex::new(ConnectorRegistryState {
                records: built_in_records(),
                source_policies: BTreeMap::new(),
                recent_nonces: BTreeMap::new(),
            }),
            store_path: None,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut records = built_in_records();
        let mut source_policies = BTreeMap::new();
        if path.exists() {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.file_type().is_file() || metadata.len() > MAX_TRUST_STORE_BYTES {
                bail!("connector trust store must be a regular file no larger than 256 KiB");
            }
            let document: TrustStoreDocument = serde_json::from_slice(&fs::read(&path)?)?;
            if document.version != TRUST_STORE_VERSION {
                bail!("unsupported connector trust-store version");
            }
            source_policies = document.source_policies;
            for mut record in document.connectors.into_values() {
                record.manifest.validate()?;
                if let Some(built_in) = records.get_mut(&record.manifest.id) {
                    built_in.enabled = record.enabled;
                    built_in.network_allowed = record.network_allowed;
                    built_in.last_event_at_unix = record.last_event_at_unix;
                    built_in.last_error = record.last_error;
                    built_in.health = if record.enabled {
                        ConnectorHealth::Ready
                    } else {
                        ConnectorHealth::Disabled
                    };
                } else {
                    if record.manifest.built_in {
                        bail!("trust store contains an unknown built-in connector identity");
                    }
                    if record.network_allowed && record.manifest.network_domains.is_empty() {
                        bail!("connector has network authority without declared domains");
                    }
                    records.insert(record.manifest.id.clone(), record);
                }
            }
            for policy in source_policies.values_mut() {
                policy.validate()?;
            }
        }
        let registry = Self {
            state: Mutex::new(ConnectorRegistryState {
                records,
                source_policies,
                recent_nonces: BTreeMap::new(),
            }),
            store_path: Some(path),
        };
        registry.persist()?;
        Ok(registry)
    }

    pub fn statuses(&self) -> Result<Vec<ConnectorStatus>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?;
        Ok(state.records.values().map(status_from_record).collect())
    }

    pub fn source_policies(&self) -> Result<Vec<crate::EventSourcePolicy>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?
            .source_policies
            .values()
            .cloned()
            .collect())
    }

    pub fn set_source_policy(
        &self,
        mut policy: crate::EventSourcePolicy,
    ) -> Result<crate::EventSourcePolicy> {
        policy.validate()?;
        {
            self.state
                .lock()
                .map_err(|_| anyhow!("connector registry unavailable"))?
                .source_policies
                .insert(policy.source, policy.clone());
        }
        self.persist()?;
        Ok(policy)
    }

    pub fn install(
        &self,
        mut manifest: ConnectorManifest,
        overwrite: bool,
    ) -> Result<ConnectorStatus> {
        manifest.validate()?;
        if manifest.built_in {
            bail!("external packages cannot claim built-in connector identity");
        }
        let connector_id = manifest.id.clone();
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("connector registry unavailable"))?;
            let rollback = match state.records.get(&connector_id) {
                Some(existing) if existing.manifest.built_in => {
                    bail!("connector id conflicts with a built-in connector")
                }
                Some(_) if !overwrite => bail!("connector already exists; update was not approved"),
                Some(existing) => Some(existing.manifest.clone()),
                None => None,
            };
            state.records.insert(
                connector_id.clone(),
                ConnectorRecord {
                    manifest,
                    enabled: false,
                    network_allowed: false,
                    health: ConnectorHealth::Disabled,
                    last_event_at_unix: None,
                    last_error: None,
                    rollback,
                },
            );
        }
        self.persist()?;
        self.status(&connector_id)
    }

    pub fn set_enabled(
        &self,
        connector_id: &str,
        enabled: bool,
        network_allowed: bool,
    ) -> Result<ConnectorStatus> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("connector registry unavailable"))?;
            let record = state
                .records
                .get_mut(connector_id)
                .ok_or_else(|| anyhow!("unknown connector: {connector_id}"))?;
            if network_allowed && record.manifest.network_domains.is_empty() {
                bail!("connector declares no network domains");
            }
            record.enabled = enabled;
            record.network_allowed = enabled && network_allowed;
            record.health = if enabled {
                ConnectorHealth::Ready
            } else {
                ConnectorHealth::Disabled
            };
            record.last_error = None;
        }
        self.persist()?;
        self.status(connector_id)
    }

    pub fn rollback(&self, connector_id: &str) -> Result<ConnectorStatus> {
        {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("connector registry unavailable"))?;
            let record = state
                .records
                .get_mut(connector_id)
                .ok_or_else(|| anyhow!("unknown connector: {connector_id}"))?;
            if record.manifest.built_in {
                bail!("built-in connectors cannot be rolled back from the trust store");
            }
            let previous = record
                .rollback
                .take()
                .ok_or_else(|| anyhow!("connector has no rollback manifest"))?;
            record.rollback = Some(std::mem::replace(&mut record.manifest, previous));
            record.enabled = false;
            record.network_allowed = false;
            record.health = ConnectorHealth::Disabled;
        }
        self.persist()?;
        self.status(connector_id)
    }

    pub fn authorize_builtin(&self, connector_id: &str, source: crate::EventSource) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?;
        let record = enabled_record(&state, connector_id, source)?;
        if !record.manifest.built_in {
            bail!("external connector requires a signed event");
        }
        Ok(())
    }

    pub fn authorize_managed_event(
        &self,
        connector_id: &str,
        source: crate::EventSource,
        payload: &Value,
    ) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?;
        let record = enabled_record(&state, connector_id, source)?;
        if !record.manifest.built_in && record.manifest.runtime.is_none() {
            bail!("connector is not managed by the connector host");
        }
        let object = payload
            .as_object()
            .ok_or_else(|| anyhow!("managed connector payload must be a JSON object"))?;
        let fields = record
            .manifest
            .event_fields
            .get(&source)
            .ok_or_else(|| anyhow!("connector has no schema for this source"))?;
        if object.keys().any(|field| !fields.contains(field)) {
            bail!("managed connector emitted a field outside its manifest schema");
        }
        Ok(())
    }

    pub fn verify_signed(&self, request: &ConnectorEventRequest, secret: &[u8]) -> Result<()> {
        let now = unix_now();
        if request.nonce.len() < 16
            || request.nonce.len() > 128
            || !request
                .nonce
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric())
            || now.abs_diff(request.timestamp_unix) > SIGNATURE_WINDOW_SECONDS
        {
            bail!("connector event nonce or timestamp is invalid");
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?;
        state
            .recent_nonces
            .retain(|_, seen| now.saturating_sub(*seen) <= SIGNATURE_WINDOW_SECONDS);
        let record = enabled_record(&state, &request.connector_id, request.source)?;
        if record.manifest.built_in {
            bail!("built-in connectors use the internal trusted path");
        }
        let signature = decode_hex(&request.signature_hex)?;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret)
            .map_err(|_| anyhow!("connector signing key is invalid"))?;
        mac.update(&signature_message(request)?);
        mac.verify_slice(&signature)
            .map_err(|_| anyhow!("connector signature verification failed"))?;
        let replay_key = (request.connector_id.clone(), request.nonce.clone());
        if state.recent_nonces.contains_key(&replay_key) {
            bail!("connector event nonce was already used");
        }
        state.recent_nonces.insert(replay_key, now);
        Ok(())
    }

    pub fn signing_key_handle(&self, connector_id: &str) -> Result<String> {
        self.state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?
            .records
            .get(connector_id)
            .and_then(|record| record.manifest.signing_key_handle.clone())
            .ok_or_else(|| anyhow!("connector has no signing-key handle"))
    }

    pub fn record_success(&self, connector_id: &str) {
        if let Ok(mut state) = self.state.lock()
            && let Some(record) = state.records.get_mut(connector_id)
        {
            record.health = ConnectorHealth::Healthy;
            record.last_event_at_unix = Some(unix_now());
            record.last_error = None;
        }
        let _ = self.persist();
    }

    pub fn record_error(&self, connector_id: &str, error: &str) {
        if let Ok(mut state) = self.state.lock()
            && let Some(record) = state.records.get_mut(connector_id)
        {
            record.health = ConnectorHealth::Error;
            record.last_error = Some(error.chars().take(500).collect());
        }
        let _ = self.persist();
    }

    fn status(&self, connector_id: &str) -> Result<ConnectorStatus> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?;
        state
            .records
            .get(connector_id)
            .map(status_from_record)
            .ok_or_else(|| anyhow!("unknown connector: {connector_id}"))
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.store_path else {
            return Ok(());
        };
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("connector registry unavailable"))?;
        let document = TrustStoreDocument {
            version: TRUST_STORE_VERSION,
            connectors: state.records.clone(),
            source_policies: state.source_policies.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document)?;
        drop(state);
        write_private_atomic_with_backup(path, &bytes)
    }
}

pub fn sign_connector_event(
    connector_id: String,
    source: crate::EventSource,
    timestamp_unix: u64,
    nonce: String,
    payload: Value,
    secret: &[u8],
) -> Result<ConnectorEventRequest> {
    let mut request = ConnectorEventRequest {
        connector_id,
        source,
        timestamp_unix,
        nonce,
        payload,
        signature_hex: String::new(),
    };
    let mut mac = Hmac::<Sha256>::new_from_slice(secret)
        .map_err(|_| anyhow!("connector signing key is invalid"))?;
    mac.update(&signature_message(&request)?);
    request.signature_hex = encode_hex(&mac.finalize().into_bytes());
    Ok(request)
}

pub fn random_connector_nonce() -> String {
    let mut bytes = [0_u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    encode_hex(&bytes)
}

fn signature_message(request: &ConnectorEventRequest) -> Result<Vec<u8>> {
    let mut message = format!(
        "{}\n{}\n{}\n{}\n",
        request.connector_id,
        request.source.as_str(),
        request.timestamp_unix,
        request.nonce
    )
    .into_bytes();
    message.extend(serde_json::to_vec(&request.payload)?);
    Ok(message)
}

fn enabled_record<'a>(
    state: &'a ConnectorRegistryState,
    connector_id: &str,
    source: crate::EventSource,
) -> Result<&'a ConnectorRecord> {
    let record = state
        .records
        .get(connector_id)
        .ok_or_else(|| anyhow!("unknown connector: {connector_id}"))?;
    if !record.enabled {
        bail!("connector is disabled");
    }
    if !record.manifest.event_sources.contains(&source) {
        bail!("connector is not authorized for this event source");
    }
    Ok(record)
}

fn status_from_record(record: &ConnectorRecord) -> ConnectorStatus {
    ConnectorStatus {
        manifest: record.manifest.clone(),
        enabled: record.enabled,
        network_allowed: record.network_allowed,
        health: record.health,
        last_event_at_unix: record.last_event_at_unix,
        last_error: record.last_error.clone(),
        rollback_available: record.rollback.is_some(),
    }
}

fn built_in_records() -> BTreeMap<String, ConnectorRecord> {
    built_in_connectors()
        .into_iter()
        .map(|manifest| {
            (
                manifest.id.clone(),
                ConnectorRecord {
                    manifest,
                    enabled: false,
                    network_allowed: false,
                    health: ConnectorHealth::Disabled,
                    last_event_at_unix: None,
                    last_error: None,
                    rollback: None,
                },
            )
        })
        .collect()
}

pub fn built_in_connectors() -> Vec<ConnectorManifest> {
    [
        (
            "desktop-events",
            "Desktop Events",
            crate::EventSource::Desktop,
        ),
        (
            "workflow-events",
            "Workflow Events",
            crate::EventSource::Workflow,
        ),
        (
            "service-health",
            "Service Health",
            crate::EventSource::ServiceHealth,
        ),
        (
            "notifications",
            "Notifications",
            crate::EventSource::Notification,
        ),
        (
            "local-calendar",
            "Local Calendar",
            crate::EventSource::Calendar,
        ),
    ]
    .into_iter()
    .map(|(id, name, source)| ConnectorManifest {
        manifest_version: CONNECTOR_MANIFEST_VERSION,
        id: id.into(),
        name: name.into(),
        description: format!("Built-in local {} adapter.", source.as_str()),
        version: "1.0.0".into(),
        event_sources: vec![source],
        event_fields: BTreeMap::from([(
            source,
            crate::supported_event_fields(source)
                .into_iter()
                .map(str::to_string)
                .collect(),
        )]),
        network_domains: Vec::new(),
        signing_key_handle: None,
        runtime: None,
        built_in: true,
    })
    .collect()
}

fn write_private_atomic_with_backup(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("connector trust-store path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    if path.exists() {
        let metadata = fs::symlink_metadata(path)?;
        if !metadata.file_type().is_file() {
            bail!("connector trust store is not a regular file");
        }
        let backup = path.with_extension("json.bak");
        if backup.exists() && !fs::symlink_metadata(&backup)?.file_type().is_file() {
            bail!("connector trust-store backup is not a regular file");
        }
        fs::copy(path, &backup)?;
        fs::set_permissions(&backup, fs::Permissions::from_mode(0o600))?;
    }
    let temp = parent.join(format!(
        ".connector-trust-{}-{:016x}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)?;
    let result = (|| -> Result<()> {
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.with_context(|| format!("persist connector trust store {}", path.display()))
}

fn validate_id(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 64
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("{kind} id must contain 1-64 lowercase ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("connector signature must be 64 hexadecimal characters");
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| anyhow!("connector signature is invalid"))
        })
        .collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn external_manifest() -> ConnectorManifest {
        ConnectorManifest {
            manifest_version: CONNECTOR_MANIFEST_VERSION,
            id: "test-calendar".into(),
            name: "Test Calendar".into(),
            description: "Test connector".into(),
            version: "1.0.0".into(),
            event_sources: vec![crate::EventSource::Calendar],
            event_fields: BTreeMap::from([(
                crate::EventSource::Calendar,
                vec!["event".into(), "title".into()],
            )]),
            network_domains: vec!["calendar.example.test".into()],
            signing_key_handle: Some("connectors/test-calendar/signing-key".into()),
            runtime: None,
            built_in: false,
        }
    }

    #[test]
    fn signed_events_are_authenticated_and_replay_protected() {
        let registry = ConnectorRegistry::memory();
        registry.install(external_manifest(), false).unwrap();
        registry.set_enabled("test-calendar", true, false).unwrap();
        let request = sign_connector_event(
            "test-calendar".into(),
            crate::EventSource::Calendar,
            unix_now(),
            "0123456789abcdef".into(),
            json!({"event":"meeting soon"}),
            b"test secret",
        )
        .unwrap();
        registry.verify_signed(&request, b"test secret").unwrap();
        assert!(registry.verify_signed(&request, b"test secret").is_err());
    }

    #[test]
    fn managed_runtime_requires_declared_sources_and_fields() {
        let registry = ConnectorRegistry::memory();
        let mut manifest = external_manifest();
        manifest.runtime = Some(ConnectorRuntimeManifest {
            executable: "/usr/libexec/focaldesk/test-calendar".into(),
            args: vec!["--poll".into()],
            poll_interval_seconds: 60,
            memory_max_mib: 64,
            cpu_quota_percent: 20,
        });
        registry.install(manifest, false).unwrap();
        registry.set_enabled("test-calendar", true, false).unwrap();
        registry
            .authorize_managed_event(
                "test-calendar",
                crate::EventSource::Calendar,
                &json!({"event":"meeting soon","title":"Example"}),
            )
            .unwrap();
        assert!(
            registry
                .authorize_managed_event(
                    "test-calendar",
                    crate::EventSource::Calendar,
                    &json!({"event":"meeting soon","private_notes":"not declared"}),
                )
                .is_err()
        );
    }

    #[test]
    fn managed_runtime_declaration_is_bounded() {
        let mut manifest = external_manifest();
        manifest.runtime = Some(ConnectorRuntimeManifest {
            executable: "relative-command".into(),
            args: Vec::new(),
            poll_interval_seconds: 1,
            memory_max_mib: 1024,
            cpu_quota_percent: 0,
        });
        assert!(manifest.validate().is_err());
    }

    #[test]
    fn builtins_are_installed_disabled_and_have_no_network_authority() {
        let statuses = ConnectorRegistry::memory().statuses().unwrap();
        assert_eq!(statuses.len(), 5);
        assert!(statuses.iter().all(|status| {
            status.manifest.built_in
                && !status.enabled
                && !status.network_allowed
                && status.manifest.network_domains.is_empty()
        }));
    }

    #[test]
    fn trust_store_round_trips_policy_with_private_recovery() {
        let root = std::env::temp_dir().join(format!(
            "focaldesk-connector-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let path = root.join("connectors.json");
        let registry = ConnectorRegistry::open(&path).unwrap();
        registry.set_enabled("service-health", true, false).unwrap();
        registry
            .set_source_policy(crate::EventSourcePolicy {
                source: crate::EventSource::ServiceHealth,
                enabled: true,
                allowed_fields: vec!["event".into(), "service".into()],
                retention_seconds: 300,
                forward_to_attention: true,
            })
            .unwrap();
        let reopened = ConnectorRegistry::open(&path).unwrap();
        assert!(
            reopened
                .statuses()
                .unwrap()
                .iter()
                .any(|status| status.manifest.id == "service-health" && status.enabled)
        );
        assert_eq!(reopened.source_policies().unwrap().len(), 1);
        assert!(path.with_extension("json.bak").exists());
        fs::remove_dir_all(root).unwrap();
    }
}
