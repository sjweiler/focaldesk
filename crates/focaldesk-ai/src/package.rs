use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const FAI_PACKAGE_VERSION: u16 = 1;
const STORE_VERSION: u16 = 1;
const MAX_BUNDLE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STORE_BYTES: usize = 32 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FaiSigner {
    pub id: String,
    pub public_key_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FaiPackageDependency {
    pub package_id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FaiPackageManifest {
    pub package_version: u16,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<FaiPackageDependency>,
    pub signer: FaiSigner,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FaiPackagePayload {
    #[serde(default)]
    pub agents: Vec<crate::AgentDefinition>,
    #[serde(default)]
    pub workflows: Vec<crate::WorkflowDefinition>,
    #[serde(default)]
    pub routines: Vec<crate::RoutineDefinition>,
    #[serde(default)]
    pub connectors: Vec<crate::ConnectorManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FaiBundle {
    pub manifest: FaiPackageManifest,
    pub payload: FaiPackagePayload,
    pub scenarios: Vec<crate::ScenarioFixture>,
    pub signature_hex: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiAuthoritySummary {
    pub tools: Vec<String>,
    pub network_domains: Vec<String>,
    pub secret_handles: Vec<String>,
    pub event_sources: Vec<crate::EventSource>,
    pub agent_triggers: usize,
    pub workflow_nodes: usize,
    pub routines: usize,
    pub connector_runtimes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiPackageInspection {
    pub package_id: String,
    pub version: String,
    pub digest_sha256: String,
    pub signature_valid: bool,
    pub signer_trusted: bool,
    pub scenarios_passed: bool,
    pub scenario_names: Vec<String>,
    pub authority: FaiAuthoritySummary,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority_delta: Option<FaiAuthoritySummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiPackageStatus {
    pub package_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub staged_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_version: Option<String>,
    pub rollback_available: bool,
    pub signer_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackageRecord {
    #[serde(default)]
    staged: Option<FaiBundle>,
    #[serde(default)]
    active: Option<FaiBundle>,
    #[serde(default)]
    rollback: Option<FaiBundle>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackageStoreDocument {
    version: u16,
    #[serde(default)]
    trusted_signers: BTreeMap<String, String>,
    #[serde(default)]
    packages: BTreeMap<String, PackageRecord>,
}

#[derive(Clone)]
struct PackageState {
    trusted_signers: BTreeMap<String, String>,
    packages: BTreeMap<String, PackageRecord>,
}

pub struct FaiPackageManager {
    state: Mutex<PackageState>,
    store_path: Option<PathBuf>,
}

impl Default for FaiPackageManager {
    fn default() -> Self {
        Self::memory()
    }
}

impl FaiPackageManager {
    pub fn memory() -> Self {
        Self {
            state: Mutex::new(PackageState {
                trusted_signers: BTreeMap::new(),
                packages: BTreeMap::new(),
            }),
            store_path: None,
        }
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let document = if path.exists() {
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("AIOS package store must be a regular file");
            }
            if metadata.len() > MAX_STORE_BYTES as u64 {
                bail!("AIOS package store exceeds 32 MiB");
            }
            let document: PackageStoreDocument = serde_json::from_slice(&fs::read(&path)?)?;
            if document.version != STORE_VERSION {
                bail!("unsupported AIOS package store version");
            }
            document
        } else {
            PackageStoreDocument {
                version: STORE_VERSION,
                trusted_signers: BTreeMap::new(),
                packages: BTreeMap::new(),
            }
        };
        let manager = Self {
            state: Mutex::new(PackageState {
                trusted_signers: document.trusted_signers,
                packages: document.packages,
            }),
            store_path: Some(path),
        };
        manager.validate_loaded()?;
        Ok(manager)
    }

    pub fn trust_signer(&self, signer: FaiSigner) -> Result<()> {
        validate_id(&signer.id, "signer")?;
        decode_public_key(&signer.public_key_hex)?;
        let previous = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("AIOS package store unavailable"))?;
            let previous = state.clone();
            state
                .trusted_signers
                .insert(signer.id, signer.public_key_hex.to_ascii_lowercase());
            previous
        };
        self.persist().inspect_err(|_| self.restore(previous))
    }

    pub fn inspect(&self, bundle: &FaiBundle) -> Result<FaiPackageInspection> {
        validate_bundle(bundle)?;
        let signature_valid = verify_signature(bundle).is_ok();
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("AIOS package store unavailable"))?;
        let signer_trusted = state
            .trusted_signers
            .get(&bundle.manifest.signer.id)
            .is_some_and(|key| key.eq_ignore_ascii_case(&bundle.manifest.signer.public_key_hex));
        let scenario_names = bundle
            .scenarios
            .iter()
            .map(|scenario| scenario.name.clone())
            .collect();
        let scenarios_passed = bundle.scenarios.iter().all(|scenario| {
            crate::evaluate_scenario(scenario.clone()).is_ok_and(|report| report.passed)
        });
        let requested_authority = authority(&bundle.payload);
        let authority_delta = state
            .packages
            .get(&bundle.manifest.id)
            .and_then(|record| record.active.as_ref())
            .map(|active| authority_delta(&authority(&active.payload), &requested_authority));
        Ok(FaiPackageInspection {
            package_id: bundle.manifest.id.clone(),
            version: bundle.manifest.version.clone(),
            digest_sha256: digest_hex(bundle)?,
            signature_valid,
            signer_trusted,
            scenarios_passed,
            scenario_names,
            authority: requested_authority,
            authority_delta,
        })
    }

    pub fn stage(&self, bundle: FaiBundle) -> Result<FaiPackageInspection> {
        let inspection = self.inspect(&bundle)?;
        if !inspection.signature_valid {
            bail!("AIOS package signature is invalid");
        }
        if !inspection.signer_trusted {
            bail!("AIOS package signer is not trusted");
        }
        if !inspection.scenarios_passed {
            bail!("AIOS package failed its Scenario Lab gate");
        }
        let package_id = bundle.manifest.id.clone();
        let previous = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("AIOS package store unavailable"))?;
            validate_dependencies(&state, &bundle)?;
            let previous = state.clone();
            state
                .packages
                .entry(package_id)
                .or_insert_with(|| PackageRecord {
                    staged: None,
                    active: None,
                    rollback: None,
                })
                .staged = Some(bundle);
            previous
        };
        self.persist().inspect_err(|_| self.restore(previous))?;
        Ok(inspection)
    }

    pub fn activate(&self, package_id: &str) -> Result<FaiBundle> {
        validate_id(package_id, "package")?;
        let (active, previous_state) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("AIOS package store unavailable"))?;
            let previous_state = state.clone();
            let record = state
                .packages
                .get_mut(package_id)
                .ok_or_else(|| anyhow!("unknown AIOS package: {package_id}"))?;
            let staged = record
                .staged
                .take()
                .ok_or_else(|| anyhow!("AIOS package has no staged version"))?;
            record.rollback = record.active.replace(staged.clone());
            (staged, previous_state)
        };
        self.persist()
            .inspect_err(|_| self.restore(previous_state))?;
        Ok(active)
    }

    pub fn rollback(&self, package_id: &str) -> Result<FaiBundle> {
        validate_id(package_id, "package")?;
        let (active, previous_state) = {
            let mut state = self
                .state
                .lock()
                .map_err(|_| anyhow!("AIOS package store unavailable"))?;
            let previous_state = state.clone();
            let record = state
                .packages
                .get_mut(package_id)
                .ok_or_else(|| anyhow!("unknown AIOS package: {package_id}"))?;
            let previous = record
                .rollback
                .take()
                .ok_or_else(|| anyhow!("AIOS package has no rollback version"))?;
            record.rollback = record.active.replace(previous.clone());
            (previous, previous_state)
        };
        self.persist()
            .inspect_err(|_| self.restore(previous_state))?;
        Ok(active)
    }

    pub fn active_bundles(&self) -> Result<Vec<FaiBundle>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| anyhow!("AIOS package store unavailable"))?
            .packages
            .values()
            .filter_map(|record| record.active.clone())
            .collect())
    }

    pub fn statuses(&self) -> Result<Vec<FaiPackageStatus>> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("AIOS package store unavailable"))?;
        Ok(state
            .packages
            .iter()
            .filter_map(|(id, record)| {
                record
                    .staged
                    .as_ref()
                    .or(record.active.as_ref())
                    .map(|bundle| FaiPackageStatus {
                        package_id: id.clone(),
                        name: bundle.manifest.name.clone(),
                        staged_version: record
                            .staged
                            .as_ref()
                            .map(|bundle| bundle.manifest.version.clone()),
                        active_version: record
                            .active
                            .as_ref()
                            .map(|bundle| bundle.manifest.version.clone()),
                        rollback_available: record.rollback.is_some(),
                        signer_id: bundle.manifest.signer.id.clone(),
                    })
            })
            .collect())
    }

    fn validate_loaded(&self) -> Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow!("AIOS package store unavailable"))?;
        for (id, key) in &state.trusted_signers {
            validate_id(id, "signer")?;
            decode_public_key(key)?;
        }
        for record in state.packages.values() {
            for bundle in [&record.staged, &record.active, &record.rollback]
                .into_iter()
                .flatten()
            {
                validate_bundle(bundle)?;
                verify_signature(bundle)?;
            }
        }
        Ok(())
    }

    fn persist(&self) -> Result<()> {
        let Some(path) = &self.store_path else {
            return Ok(());
        };
        let bytes = {
            let state = self
                .state
                .lock()
                .map_err(|_| anyhow!("AIOS package store unavailable"))?;
            serde_json::to_vec_pretty(&PackageStoreDocument {
                version: STORE_VERSION,
                trusted_signers: state.trusted_signers.clone(),
                packages: state.packages.clone(),
            })?
        };
        if bytes.len() > MAX_STORE_BYTES {
            bail!("AIOS package store exceeds 32 MiB");
        }
        write_private_atomic(path, &bytes)
    }

    fn restore(&self, previous: PackageState) {
        if let Ok(mut state) = self.state.lock() {
            *state = previous;
        }
    }
}

pub fn sign_fai_bundle(bundle: &mut FaiBundle, secret_key: &[u8; 32]) -> Result<()> {
    let signing_key = SigningKey::from_bytes(secret_key);
    let public_key = signing_key.verifying_key();
    bundle.manifest.signer.public_key_hex = encode_hex(public_key.as_bytes());
    validate_bundle_without_signature(bundle)?;
    bundle.signature_hex = encode_hex(&signing_key.sign(&signing_bytes(bundle)?).to_bytes());
    Ok(())
}

pub fn fai_signer_public_key_hex(secret_key: &[u8; 32]) -> String {
    let signing_key = SigningKey::from_bytes(secret_key);
    encode_hex(signing_key.verifying_key().as_bytes())
}

pub fn fai_signer_secret_handle(signer_id: &str) -> Result<String> {
    validate_id(signer_id, "signer")?;
    Ok(format!("aios/signers/{signer_id}/ed25519"))
}

fn validate_bundle(bundle: &FaiBundle) -> Result<()> {
    validate_bundle_without_signature(bundle)?;
    if bundle.signature_hex.len() != 128 {
        bail!("AIOS package signature must be 64-byte hexadecimal Ed25519 data");
    }
    if serde_json::to_vec(bundle)?.len() > MAX_BUNDLE_BYTES {
        bail!("AIOS package exceeds 2 MiB");
    }
    Ok(())
}

fn validate_bundle_without_signature(bundle: &FaiBundle) -> Result<()> {
    let manifest = &bundle.manifest;
    if manifest.package_version != FAI_PACKAGE_VERSION {
        bail!("unsupported AIOS package version");
    }
    validate_id(&manifest.id, "package")?;
    validate_id(&manifest.signer.id, "signer")?;
    decode_public_key(&manifest.signer.public_key_hex)?;
    if manifest.name.trim().is_empty()
        || manifest.name.chars().count() > 100
        || manifest.version.trim().is_empty()
        || manifest.version.chars().count() > 40
        || manifest.description.chars().count() > 1_000
        || manifest.dependencies.len() > 32
        || bundle.scenarios.is_empty()
        || bundle.scenarios.len() > 32
    {
        bail!("AIOS package metadata is out of bounds");
    }
    let mut ids = BTreeSet::new();
    for dependency in &manifest.dependencies {
        validate_id(&dependency.package_id, "dependency")?;
        if dependency.version.trim().is_empty()
            || dependency.version.len() > 40
            || !ids.insert(&dependency.package_id)
        {
            bail!("AIOS package dependency is invalid or duplicated");
        }
    }
    validate_payload(&bundle.payload)?;
    for mut scenario in bundle.scenarios.clone() {
        scenario.validate()?;
    }
    Ok(())
}

fn validate_payload(payload: &FaiPackagePayload) -> Result<()> {
    if payload.agents.len() > 64
        || payload.workflows.len() > 64
        || payload.routines.len() > 64
        || payload.connectors.len() > 32
        || payload.agents.is_empty()
            && payload.workflows.is_empty()
            && payload.routines.is_empty()
            && payload.connectors.is_empty()
    {
        bail!("AIOS package payload is empty or exceeds its bounds");
    }
    let mut ids = BTreeSet::new();
    for item in &payload.agents {
        item.validate()?;
        if item.built_in || !ids.insert(format!("agent:{}", item.id)) {
            bail!("package agents cannot claim built-in or duplicate identities");
        }
    }
    for item in &payload.workflows {
        item.validate()?;
        if item.built_in || !ids.insert(format!("workflow:{}", item.id)) {
            bail!("package workflows cannot claim built-in or duplicate identities");
        }
    }
    for item in &payload.routines {
        item.validate()?;
        if !ids.insert(format!("routine:{}", item.id)) {
            bail!("package routines cannot duplicate identities");
        }
    }
    for item in &payload.connectors {
        let mut item = item.clone();
        item.validate()?;
        if item.built_in || !ids.insert(format!("connector:{}", item.id)) {
            bail!("package connectors cannot claim built-in or duplicate identities");
        }
    }
    Ok(())
}

fn verify_signature(bundle: &FaiBundle) -> Result<()> {
    let key =
        VerifyingKey::from_bytes(&decode_public_key(&bundle.manifest.signer.public_key_hex)?)?;
    let signature_bytes = decode_hex(&bundle.signature_hex)?;
    let signature = Signature::from_slice(&signature_bytes)?;
    key.verify(&signing_bytes(bundle)?, &signature)
        .context("AIOS package signature verification failed")
}

fn signing_bytes(bundle: &FaiBundle) -> Result<Vec<u8>> {
    #[derive(Serialize)]
    struct Signed<'a> {
        manifest: &'a FaiPackageManifest,
        payload: &'a FaiPackagePayload,
        scenarios: &'a [crate::ScenarioFixture],
    }
    Ok(serde_json::to_vec(&Signed {
        manifest: &bundle.manifest,
        payload: &bundle.payload,
        scenarios: &bundle.scenarios,
    })?)
}

fn digest_hex(bundle: &FaiBundle) -> Result<String> {
    Ok(encode_hex(&Sha256::digest(signing_bytes(bundle)?)))
}

fn authority(payload: &FaiPackagePayload) -> FaiAuthoritySummary {
    let mut tools = BTreeSet::new();
    let mut domains = BTreeSet::new();
    let mut secrets = BTreeSet::new();
    let mut sources = BTreeSet::new();
    for agent in &payload.agents {
        tools.extend(agent.tool_allowlist.iter().cloned());
        if let Some(policy) = &agent.capability_policy {
            collect_capability_authority(policy, &mut domains, &mut secrets);
        }
    }
    for workflow in &payload.workflows {
        if let Some(policy) = &workflow.capability_ceiling {
            collect_capability_authority(policy, &mut domains, &mut secrets);
        }
    }
    for connector in &payload.connectors {
        domains.extend(connector.network_domains.iter().cloned());
        sources.extend(connector.event_sources.iter().copied());
        if let Some(handle) = &connector.signing_key_handle {
            secrets.insert(handle.clone());
        }
    }
    FaiAuthoritySummary {
        tools: tools.into_iter().collect(),
        network_domains: domains.into_iter().collect(),
        secret_handles: secrets.into_iter().collect(),
        event_sources: sources.into_iter().collect(),
        agent_triggers: payload
            .agents
            .iter()
            .map(|agent| agent.triggers.len())
            .sum(),
        workflow_nodes: payload
            .workflows
            .iter()
            .map(|workflow| workflow.nodes.len())
            .sum(),
        routines: payload.routines.len(),
        connector_runtimes: payload
            .connectors
            .iter()
            .filter(|connector| connector.runtime.is_some())
            .count(),
    }
}

fn collect_capability_authority(
    policy: &crate::CapabilityPolicy,
    domains: &mut BTreeSet<String>,
    secrets: &mut BTreeSet<String>,
) {
    if let Some(origins) = &policy.network_origins {
        domains.extend(origins.iter().cloned());
    }
    if let Some(handles) = &policy.secret_handles {
        secrets.extend(handles.iter().cloned());
    }
}

fn authority_delta(old: &FaiAuthoritySummary, new: &FaiAuthoritySummary) -> FaiAuthoritySummary {
    FaiAuthoritySummary {
        tools: difference(&old.tools, &new.tools),
        network_domains: difference(&old.network_domains, &new.network_domains),
        secret_handles: difference(&old.secret_handles, &new.secret_handles),
        event_sources: new
            .event_sources
            .iter()
            .filter(|value| !old.event_sources.contains(value))
            .copied()
            .collect(),
        agent_triggers: new.agent_triggers.saturating_sub(old.agent_triggers),
        workflow_nodes: new.workflow_nodes.saturating_sub(old.workflow_nodes),
        routines: new.routines.saturating_sub(old.routines),
        connector_runtimes: new
            .connector_runtimes
            .saturating_sub(old.connector_runtimes),
    }
}

fn difference(old: &[String], new: &[String]) -> Vec<String> {
    new.iter()
        .filter(|value| !old.contains(value))
        .cloned()
        .collect()
}

fn validate_dependencies(state: &PackageState, bundle: &FaiBundle) -> Result<()> {
    for dependency in &bundle.manifest.dependencies {
        let installed = state
            .packages
            .get(&dependency.package_id)
            .and_then(|record| record.active.as_ref())
            .ok_or_else(|| {
                anyhow!(
                    "missing active package dependency: {}",
                    dependency.package_id
                )
            })?;
        if installed.manifest.version != dependency.version {
            bail!(
                "package dependency {} requires version {} but {} is active",
                dependency.package_id,
                dependency.version,
                installed.manifest.version
            );
        }
    }
    Ok(())
}

fn validate_id(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 80
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("invalid AIOS {kind} id");
    }
    Ok(())
}

fn decode_public_key(value: &str) -> Result<[u8; 32]> {
    decode_hex(value)?
        .try_into()
        .map_err(|_| anyhow!("Ed25519 public key must be 32 bytes"))
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid hexadecimal data");
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(Into::into))
        .collect()
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("package store has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("package store file name is invalid"))?;
    let temp = parent.join(format!(".{file_name}.{:016x}.tmp", rand::random::<u64>()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AgentDefinition, ScenarioInitialState, ScenarioStep};

    fn bundle(version: &str) -> FaiBundle {
        FaiBundle {
            manifest: FaiPackageManifest {
                package_version: FAI_PACKAGE_VERSION,
                id: "focus-kit".into(),
                name: "Focus Kit".into(),
                version: version.into(),
                description: "A signed package".into(),
                dependencies: Vec::new(),
                signer: FaiSigner {
                    id: "local-dev".into(),
                    public_key_hex: "00".repeat(32),
                },
            },
            payload: FaiPackagePayload {
                agents: vec![AgentDefinition {
                    manifest_version: crate::AGENT_MANIFEST_VERSION,
                    id: "focus-guide".into(),
                    name: "Focus Guide".into(),
                    description: "Helps plan focus work".into(),
                    instructions: "Suggest a bounded plan.".into(),
                    tool_allowlist: vec!["get_session_status".into()],
                    max_tool_steps: 2,
                    max_context_chars: 8_000,
                    max_output_tokens: 256,
                    timeout_seconds: 60,
                    memory: false,
                    voice: false,
                    triggers: Vec::new(),
                    daily_token_limit: None,
                    daily_cost_limit_microusd: None,
                    input_cost_microusd_per_million: None,
                    output_cost_microusd_per_million: None,
                    capability_policy: None,
                    built_in: false,
                }],
                ..Default::default()
            },
            scenarios: vec![crate::ScenarioFixture {
                scenario_version: crate::SCENARIO_VERSION,
                name: "safe install".into(),
                description: "No live mutation".into(),
                synthetic: true,
                initial: ScenarioInitialState::default(),
                steps: vec![ScenarioStep::VoicePhrase {
                    text: "open settings".into(),
                    expected_destination: None,
                }],
                expected_violation_codes: Vec::new(),
            }],
            signature_hex: String::new(),
        }
    }

    #[test]
    fn signed_package_requires_explicit_trust_and_supports_rollback() {
        let manager = FaiPackageManager::memory();
        let secret = [7_u8; 32];
        let mut first = bundle("1.0.0");
        sign_fai_bundle(&mut first, &secret).unwrap();
        assert!(!manager.inspect(&first).unwrap().signer_trusted);
        assert!(manager.stage(first.clone()).is_err());
        manager.trust_signer(first.manifest.signer.clone()).unwrap();
        manager.stage(first).unwrap();
        assert_eq!(
            manager.activate("focus-kit").unwrap().manifest.version,
            "1.0.0"
        );

        let mut second = bundle("2.0.0");
        sign_fai_bundle(&mut second, &secret).unwrap();
        manager.stage(second).unwrap();
        manager.activate("focus-kit").unwrap();
        assert_eq!(
            manager.rollback("focus-kit").unwrap().manifest.version,
            "1.0.0"
        );
    }

    #[test]
    fn tampering_is_detected() {
        let mut package = bundle("1.0.0");
        sign_fai_bundle(&mut package, &[9_u8; 32]).unwrap();
        package.manifest.description = "tampered".into();
        let inspection = FaiPackageManager::memory().inspect(&package).unwrap();
        assert!(!inspection.signature_valid);
    }

    #[test]
    fn authority_includes_agent_and_workflow_capability_scopes() {
        let mut package = bundle("1.0.0");
        package.payload.agents[0].capability_policy = Some(crate::CapabilityPolicy {
            network_origins: Some(vec!["http://127.0.0.1:11434".into()]),
            secret_handles: Some(vec!["local/model-token".into()]),
            ..Default::default()
        });
        package.payload.workflows.push(crate::WorkflowDefinition {
            manifest_version: crate::WORKFLOW_MANIFEST_VERSION,
            id: "focus-start".into(),
            name: "Focus Start".into(),
            description: "Build a bounded focus brief.".into(),
            nodes: vec![crate::WorkflowNode {
                id: "observe".into(),
                agent_id: "focus-guide".into(),
                objective: "Observe the current session.".into(),
                depends_on: Vec::new(),
            }],
            max_parallelism: 1,
            timeout_seconds: 60,
            max_total_tokens: 1_000,
            capability_ceiling: Some(crate::CapabilityPolicy {
                network_origins: Some(vec!["https://models.example".into()]),
                secret_handles: Some(vec!["workflow/model-token".into()]),
                ..Default::default()
            }),
            built_in: false,
        });

        let authority = authority(&package.payload);
        assert_eq!(
            authority.network_domains,
            vec![
                "http://127.0.0.1:11434".to_string(),
                "https://models.example".to_string()
            ]
        );
        assert_eq!(
            authority.secret_handles,
            vec![
                "local/model-token".to_string(),
                "workflow/model-token".to_string()
            ]
        );
    }

    #[test]
    fn scenario_gate_rejects_unexpected_authority_violation() {
        let manager = FaiPackageManager::memory();
        let mut package = bundle("1.0.0");
        package.scenarios[0].steps = vec![ScenarioStep::AgentPlan {
            agent_id: "focus-guide".into(),
            tools: vec!["get_session_status".into()],
            proposed_mutation: true,
            confirmation_present: false,
            lease: crate::ScenarioLeaseState::Active,
            estimated_tokens: 100,
        }];
        sign_fai_bundle(&mut package, &[11_u8; 32]).unwrap();
        manager
            .trust_signer(package.manifest.signer.clone())
            .unwrap();
        assert!(!manager.inspect(&package).unwrap().scenarios_passed);
        assert!(manager.stage(package).is_err());
    }

    #[test]
    fn persistent_store_is_private_and_reopens() {
        let directory = std::env::temp_dir().join(format!(
            "focaldesk-fai-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let path = directory.join("packages.json");
        let manager = FaiPackageManager::open(&path).unwrap();
        let mut package = bundle("1.0.0");
        sign_fai_bundle(&mut package, &[13_u8; 32]).unwrap();
        manager
            .trust_signer(package.manifest.signer.clone())
            .unwrap();
        manager.stage(package).unwrap();
        manager.activate("focus-kit").unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let reopened = FaiPackageManager::open(&path).unwrap();
        assert_eq!(reopened.active_bundles().unwrap().len(), 1);
        fs::remove_dir_all(directory).unwrap();
    }
}
