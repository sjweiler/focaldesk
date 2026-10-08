use anyhow::{Context, Result, anyhow, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const PRIVATE_REGISTRY_VERSION: u16 = 1;
const MAX_POLICY_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FaiRegistryPolicy {
    pub version: u16,
    pub registry_id: String,
    #[serde(default)]
    pub approved_signers: BTreeMap<String, String>,
    #[serde(default)]
    pub revoked: Vec<FaiCatalogRevocation>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FaiCatalogRevocation {
    pub package_id: String,
    pub version: String,
    pub reason: String,
    pub revoked_at_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiCatalogEntry {
    pub package_id: String,
    pub name: String,
    pub version: String,
    pub digest_sha256: String,
    pub signer_id: String,
    pub signer_public_key_hex: String,
    pub dependencies: Vec<crate::FaiPackageDependency>,
    pub authority: crate::FaiAuthoritySummary,
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiRegistryCatalog {
    pub version: u16,
    pub registry_id: String,
    pub sequence: u64,
    pub generated_at_unix: u64,
    pub packages: Vec<FaiCatalogEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiSignedCatalog {
    pub catalog: FaiRegistryCatalog,
    pub signing_public_key_hex: String,
    pub signature_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiPackageLockEntry {
    pub package_id: String,
    pub version: String,
    pub digest_sha256: String,
    pub signer_public_key_hex: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiPackageLock {
    pub version: u16,
    pub registry_id: String,
    pub catalog_sequence: u64,
    pub packages: Vec<FaiPackageLockEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiRegistryCompatibility {
    pub package_id: String,
    pub from_version: String,
    pub to_version: String,
    pub authority_added: crate::FaiAuthoritySummary,
    pub authority_removed: crate::FaiAuthoritySummary,
    pub dependencies_changed: bool,
    pub requires_authority_review: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RegistryState {
    version: u16,
    sequence: u64,
}

pub struct FaiPrivateRegistry {
    root: PathBuf,
    policy_path: PathBuf,
    signing_key: SigningKey,
}

impl FaiPrivateRegistry {
    pub fn open(root: impl AsRef<Path>, signing_secret: &[u8; 32]) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        fs::create_dir_all(root.join("packages"))?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(root.join("packages"), fs::Permissions::from_mode(0o700))?;
        Ok(Self {
            policy_path: root.join("policy.json"),
            root,
            signing_key: SigningKey::from_bytes(signing_secret),
        })
    }

    pub fn signing_public_key_hex(&self) -> String {
        encode_hex(self.signing_key.verifying_key().as_bytes())
    }

    pub fn initialize_policy(&self, registry_id: &str) -> Result<FaiRegistryPolicy> {
        validate_id(registry_id, "registry")?;
        if self.policy_path.exists() {
            return self.policy();
        }
        let policy = FaiRegistryPolicy {
            version: PRIVATE_REGISTRY_VERSION,
            registry_id: registry_id.to_string(),
            approved_signers: BTreeMap::new(),
            revoked: Vec::new(),
        };
        write_private_atomic(&self.policy_path, &serde_json::to_vec_pretty(&policy)?)?;
        self.write_state(RegistryState {
            version: PRIVATE_REGISTRY_VERSION,
            sequence: 0,
        })?;
        Ok(policy)
    }

    pub fn policy(&self) -> Result<FaiRegistryPolicy> {
        let metadata = fs::symlink_metadata(&self.policy_path)
            .context("private registry policy is not initialized")?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_POLICY_BYTES
        {
            bail!("private registry policy file is unsafe or oversized");
        }
        let policy: FaiRegistryPolicy = serde_json::from_slice(&fs::read(&self.policy_path)?)?;
        validate_policy(&policy)?;
        Ok(policy)
    }

    pub fn approve_signer(&self, signer: crate::FaiSigner) -> Result<FaiRegistryPolicy> {
        validate_id(&signer.id, "signer")?;
        decode_fixed::<32>(&signer.public_key_hex)?;
        let mut policy = self.policy()?;
        let previous = policy.clone();
        policy
            .approved_signers
            .insert(signer.id, signer.public_key_hex.to_ascii_lowercase());
        write_private_atomic(&self.policy_path, &serde_json::to_vec_pretty(&policy)?)?;
        if let Err(error) = self.bump_sequence() {
            let _ = write_private_atomic(&self.policy_path, &serde_json::to_vec_pretty(&previous)?);
            return Err(error);
        }
        Ok(policy)
    }

    pub fn publish(&self, bundle: &crate::FaiBundle) -> Result<FaiCatalogEntry> {
        let policy = self.policy()?;
        let inspection = crate::FaiPackageManager::memory().inspect(bundle)?;
        if !inspection.signature_valid || !inspection.scenarios_passed {
            bail!("registry publish requires a valid signature and passing scenarios");
        }
        if policy
            .approved_signers
            .get(&bundle.manifest.signer.id)
            .is_none_or(|key| !key.eq_ignore_ascii_case(&bundle.manifest.signer.public_key_hex))
        {
            bail!("package signer is not approved by registry policy");
        }
        if policy.revoked.iter().any(|item| {
            item.package_id == bundle.manifest.id && item.version == bundle.manifest.version
        }) {
            bail!("revoked package coordinates cannot be republished");
        }
        validate_version(&bundle.manifest.version)?;
        let path = self.package_path(&bundle.manifest.id, &bundle.manifest.version)?;
        if path.exists() {
            bail!("registry package version is immutable and already exists");
        }
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("package path has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        write_new_private(&path, &serde_json::to_vec_pretty(bundle)?)?;
        if let Err(error) = self.bump_sequence() {
            let _ = fs::remove_file(&path);
            return Err(error);
        }
        self.entry(bundle, &policy, inspection)
    }

    pub fn revoke(
        &self,
        package_id: &str,
        version: &str,
        reason: &str,
    ) -> Result<FaiCatalogRevocation> {
        validate_id(package_id, "package")?;
        validate_version(version)?;
        if reason.trim().is_empty() || reason.chars().count() > 500 {
            bail!("revocation reason is empty or too long");
        }
        if !self.package_path(package_id, version)?.exists() {
            bail!("cannot revoke a package version that is not published");
        }
        let mut policy = self.policy()?;
        let previous = policy.clone();
        if policy
            .revoked
            .iter()
            .any(|item| item.package_id == package_id && item.version == version)
        {
            bail!("package version is already revoked");
        }
        let revocation = FaiCatalogRevocation {
            package_id: package_id.to_string(),
            version: version.to_string(),
            reason: reason.trim().to_string(),
            revoked_at_unix: unix_now(),
        };
        policy.revoked.push(revocation.clone());
        write_private_atomic(&self.policy_path, &serde_json::to_vec_pretty(&policy)?)?;
        if let Err(error) = self.bump_sequence() {
            let _ = write_private_atomic(&self.policy_path, &serde_json::to_vec_pretty(&previous)?);
            return Err(error);
        }
        Ok(revocation)
    }

    pub fn catalog(&self) -> Result<FaiSignedCatalog> {
        let policy = self.policy()?;
        let state = self.state()?;
        let mut packages = Vec::new();
        let packages_root = self.root.join("packages");
        for package_dir in fs::read_dir(packages_root)? {
            let package_dir = package_dir?;
            if package_dir.file_type()?.is_symlink() || !package_dir.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(package_dir.path())? {
                let file = file?;
                if file.file_type()?.is_symlink()
                    || !file.file_type()?.is_file()
                    || file.metadata()?.len() > 2 * 1024 * 1024
                {
                    continue;
                }
                let bundle: crate::FaiBundle = serde_json::from_slice(&fs::read(file.path())?)?;
                let inspection = crate::FaiPackageManager::memory().inspect(&bundle)?;
                if !inspection.signature_valid || !inspection.scenarios_passed {
                    bail!("registry contains an invalid package");
                }
                packages.push(self.entry(&bundle, &policy, inspection)?);
            }
        }
        packages.sort_by(|left, right| {
            left.package_id
                .cmp(&right.package_id)
                .then(left.version.cmp(&right.version))
        });
        let catalog = FaiRegistryCatalog {
            version: PRIVATE_REGISTRY_VERSION,
            registry_id: policy.registry_id,
            sequence: state.sequence,
            generated_at_unix: unix_now(),
            packages,
        };
        let signature = self.signing_key.sign(&serde_json::to_vec(&catalog)?);
        Ok(FaiSignedCatalog {
            catalog,
            signing_public_key_hex: self.signing_public_key_hex(),
            signature_hex: encode_hex(&signature.to_bytes()),
        })
    }

    pub fn package(&self, package_id: &str, version: &str) -> Result<crate::FaiBundle> {
        let policy = self.policy()?;
        if policy
            .revoked
            .iter()
            .any(|item| item.package_id == package_id && item.version == version)
        {
            bail!("package version is revoked");
        }
        let path = self.package_path(package_id, version)?;
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > 2 * 1024 * 1024
        {
            bail!("registry package file is unsafe or oversized");
        }
        let bundle: crate::FaiBundle = serde_json::from_slice(&fs::read(path)?)?;
        let inspection = crate::FaiPackageManager::memory().inspect(&bundle)?;
        if !inspection.signature_valid || !inspection.scenarios_passed {
            bail!("registry package verification failed");
        }
        Ok(bundle)
    }

    fn entry(
        &self,
        bundle: &crate::FaiBundle,
        policy: &FaiRegistryPolicy,
        inspection: crate::FaiPackageInspection,
    ) -> Result<FaiCatalogEntry> {
        let revocation = policy.revoked.iter().find(|item| {
            item.package_id == bundle.manifest.id && item.version == bundle.manifest.version
        });
        Ok(FaiCatalogEntry {
            package_id: bundle.manifest.id.clone(),
            name: bundle.manifest.name.clone(),
            version: bundle.manifest.version.clone(),
            digest_sha256: inspection.digest_sha256,
            signer_id: bundle.manifest.signer.id.clone(),
            signer_public_key_hex: bundle.manifest.signer.public_key_hex.clone(),
            dependencies: bundle.manifest.dependencies.clone(),
            authority: inspection.authority,
            revoked: revocation.is_some(),
            revocation_reason: revocation.map(|item| item.reason.clone()),
        })
    }

    fn package_path(&self, package_id: &str, version: &str) -> Result<PathBuf> {
        validate_id(package_id, "package")?;
        validate_version(version)?;
        Ok(self
            .root
            .join("packages")
            .join(package_id)
            .join(format!("{version}.fai")))
    }

    fn state(&self) -> Result<RegistryState> {
        let state: RegistryState =
            serde_json::from_slice(&fs::read(self.root.join("state.json"))?)?;
        if state.version != PRIVATE_REGISTRY_VERSION {
            bail!("unsupported private registry state version");
        }
        Ok(state)
    }

    fn write_state(&self, state: RegistryState) -> Result<()> {
        write_private_atomic(
            &self.root.join("state.json"),
            &serde_json::to_vec_pretty(&state)?,
        )
    }

    fn bump_sequence(&self) -> Result<()> {
        let mut state = self.state()?;
        state.sequence = state.sequence.saturating_add(1);
        self.write_state(state)
    }
}

pub fn verify_registry_catalog(
    signed: &FaiSignedCatalog,
    trusted_public_key_hex: &str,
    previous_sequence: Option<u64>,
) -> Result<()> {
    if signed.catalog.version != PRIVATE_REGISTRY_VERSION {
        bail!("unsupported private registry catalog version");
    }
    if !signed
        .signing_public_key_hex
        .eq_ignore_ascii_case(trusted_public_key_hex)
    {
        bail!("registry catalog key does not match the pinned key");
    }
    if previous_sequence.is_some_and(|sequence| signed.catalog.sequence < sequence) {
        bail!("registry catalog sequence rolled back");
    }
    let key = VerifyingKey::from_bytes(&decode_fixed::<32>(trusted_public_key_hex)?)?;
    let signature = Signature::from_slice(&decode_fixed::<64>(&signed.signature_hex)?)?;
    key.verify(&serde_json::to_vec(&signed.catalog)?, &signature)
        .context("registry catalog signature verification failed")?;
    let mut coordinates = BTreeSet::new();
    for entry in &signed.catalog.packages {
        validate_id(&entry.package_id, "package")?;
        validate_version(&entry.version)?;
        decode_fixed::<32>(&entry.digest_sha256)?;
        decode_fixed::<32>(&entry.signer_public_key_hex)?;
        if !coordinates.insert((&entry.package_id, &entry.version)) {
            bail!("registry catalog contains duplicate package coordinates");
        }
    }
    Ok(())
}

pub fn resolve_catalog_lock(
    signed: &FaiSignedCatalog,
    package_id: &str,
    version: &str,
) -> Result<FaiPackageLock> {
    let index = signed
        .catalog
        .packages
        .iter()
        .map(|entry| ((entry.package_id.as_str(), entry.version.as_str()), entry))
        .collect::<BTreeMap<_, _>>();
    let mut pending = vec![(package_id.to_string(), version.to_string())];
    let mut selected = BTreeMap::<String, FaiPackageLockEntry>::new();
    while let Some((id, version)) = pending.pop() {
        if let Some(existing) = selected.get(&id) {
            if existing.version != version {
                bail!("dependency graph requires conflicting exact versions for {id}");
            }
            continue;
        }
        let entry = index
            .get(&(id.as_str(), version.as_str()))
            .ok_or_else(|| anyhow!("catalog is missing dependency {id} {version}"))?;
        if entry.revoked {
            bail!("dependency {id} {version} is revoked");
        }
        for dependency in &entry.dependencies {
            pending.push((dependency.package_id.clone(), dependency.version.clone()));
        }
        let digest_sha256 = entry.digest_sha256.clone();
        let signer_public_key_hex = entry.signer_public_key_hex.clone();
        selected.insert(
            id.clone(),
            FaiPackageLockEntry {
                package_id: id,
                version,
                digest_sha256,
                signer_public_key_hex,
            },
        );
    }
    Ok(FaiPackageLock {
        version: PRIVATE_REGISTRY_VERSION,
        registry_id: signed.catalog.registry_id.clone(),
        catalog_sequence: signed.catalog.sequence,
        packages: selected.into_values().collect(),
    })
}

pub fn compare_catalog_versions(
    catalog: &FaiSignedCatalog,
    package_id: &str,
    from_version: &str,
    to_version: &str,
) -> Result<FaiRegistryCompatibility> {
    let find = |version: &str| {
        catalog
            .catalog
            .packages
            .iter()
            .find(|entry| entry.package_id == package_id && entry.version == version)
            .ok_or_else(|| anyhow!("catalog does not contain {package_id} {version}"))
    };
    let from = find(from_version)?;
    let to = find(to_version)?;
    let added = authority_difference(&from.authority, &to.authority);
    let removed = authority_difference(&to.authority, &from.authority);
    let requires_authority_review = !authority_is_empty(&added);
    Ok(FaiRegistryCompatibility {
        package_id: package_id.to_string(),
        from_version: from_version.to_string(),
        to_version: to_version.to_string(),
        authority_added: added,
        authority_removed: removed,
        dependencies_changed: from.dependencies != to.dependencies,
        requires_authority_review,
    })
}

pub async fn fetch_registry_catalog(
    base_url: &str,
    bearer_token: &str,
    trusted_public_key_hex: &str,
    previous_sequence: Option<u64>,
) -> Result<FaiSignedCatalog> {
    let response = registry_http_client()?
        .get(registry_url(base_url, "v1/catalog")?)
        .bearer_auth(bearer_token)
        .send()
        .await
        .context("fetch private registry catalog")?;
    if !response.status().is_success() {
        bail!(
            "private registry catalog request failed with HTTP {}",
            response.status()
        );
    }
    if response
        .content_length()
        .is_some_and(|size| size > 4 * 1024 * 1024)
    {
        bail!("private registry catalog exceeds 4 MiB");
    }
    let bytes = response.bytes().await?;
    if bytes.len() > 4 * 1024 * 1024 {
        bail!("private registry catalog exceeds 4 MiB");
    }
    let catalog: FaiSignedCatalog = serde_json::from_slice(&bytes)?;
    verify_registry_catalog(&catalog, trusted_public_key_hex, previous_sequence)?;
    Ok(catalog)
}

pub async fn download_registry_package(
    base_url: &str,
    bearer_token: &str,
    catalog: &FaiSignedCatalog,
    package_id: &str,
    version: &str,
) -> Result<crate::FaiBundle> {
    let entry = catalog
        .catalog
        .packages
        .iter()
        .find(|entry| entry.package_id == package_id && entry.version == version)
        .ok_or_else(|| anyhow!("package version is absent from the verified catalog"))?;
    if entry.revoked {
        bail!("package version is revoked and remains quarantined");
    }
    let response = registry_http_client()?
        .get(registry_url(
            base_url,
            &format!("v1/packages/{package_id}/{version}"),
        )?)
        .bearer_auth(bearer_token)
        .send()
        .await
        .context("download private registry package")?;
    if !response.status().is_success() {
        bail!(
            "private registry download failed with HTTP {}",
            response.status()
        );
    }
    if response
        .content_length()
        .is_some_and(|size| size > 2 * 1024 * 1024)
    {
        bail!("private registry package exceeds 2 MiB");
    }
    let bytes = response.bytes().await?;
    if bytes.len() > 2 * 1024 * 1024 {
        bail!("private registry package exceeds 2 MiB");
    }
    let bundle: crate::FaiBundle = serde_json::from_slice(&bytes)?;
    if bundle.manifest.id != package_id || bundle.manifest.version != version {
        bail!("downloaded package coordinates do not match the request");
    }
    let inspection = crate::FaiPackageManager::memory().inspect(&bundle)?;
    if !inspection.signature_valid
        || !inspection.scenarios_passed
        || inspection.digest_sha256 != entry.digest_sha256
        || inspection.authority != entry.authority
        || bundle.manifest.signer.id != entry.signer_id
        || !bundle
            .manifest
            .signer
            .public_key_hex
            .eq_ignore_ascii_case(&entry.signer_public_key_hex)
    {
        bail!("downloaded package failed catalog, signature, or scenario verification");
    }
    Ok(bundle)
}

pub async fn publish_registry_package(
    base_url: &str,
    bearer_token: &str,
    bundle: &crate::FaiBundle,
) -> Result<FaiCatalogEntry> {
    let inspection = crate::FaiPackageManager::memory().inspect(bundle)?;
    if !inspection.signature_valid || !inspection.scenarios_passed {
        bail!("refusing to publish an invalid package");
    }
    let response = registry_http_client()?
        .put(registry_url(base_url, "v1/packages")?)
        .bearer_auth(bearer_token)
        .json(bundle)
        .send()
        .await
        .context("publish private registry package")?;
    if !response.status().is_success() {
        bail!(
            "private registry publish failed with HTTP {}",
            response.status()
        );
    }
    let bytes = response.bytes().await?;
    if bytes.len() > 256 * 1024 {
        bail!("private registry publish response is oversized");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn approve_registry_signer(
    base_url: &str,
    bearer_token: &str,
    signer: &crate::FaiSigner,
) -> Result<FaiRegistryPolicy> {
    let response = registry_http_client()?
        .post(registry_url(base_url, "v1/signers")?)
        .bearer_auth(bearer_token)
        .json(signer)
        .send()
        .await
        .context("approve private registry signer")?;
    if !response.status().is_success() {
        bail!(
            "private registry signer approval failed with HTTP {}",
            response.status()
        );
    }
    let bytes = response.bytes().await?;
    if bytes.len() > 512 * 1024 {
        bail!("private registry policy response is oversized");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

pub async fn revoke_registry_package(
    base_url: &str,
    bearer_token: &str,
    package_id: &str,
    version: &str,
    reason: &str,
) -> Result<FaiCatalogRevocation> {
    let response = registry_http_client()?
        .post(registry_url(base_url, "v1/revoke")?)
        .bearer_auth(bearer_token)
        .json(&serde_json::json!({
            "package_id": package_id,
            "version": version,
            "reason": reason,
        }))
        .send()
        .await
        .context("revoke private registry package")?;
    if !response.status().is_success() {
        bail!(
            "private registry revocation failed with HTTP {}",
            response.status()
        );
    }
    let bytes = response.bytes().await?;
    if bytes.len() > 256 * 1024 {
        bail!("private registry revocation response is oversized");
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn registry_http_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()?)
}

fn registry_url(base_url: &str, suffix: &str) -> Result<url::Url> {
    let mut base = url::Url::parse(base_url)?;
    let loopback = base
        .host_str()
        .is_some_and(|host| matches!(host, "127.0.0.1" | "::1" | "localhost"));
    if base.scheme() != "https" && !(base.scheme() == "http" && loopback) {
        bail!("private registry requires HTTPS except on loopback");
    }
    if base.username() != ""
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        bail!("private registry URL must not contain credentials, query, or fragment");
    }
    if !base.path().ends_with('/') {
        base.set_path(&format!("{}/", base.path()));
    }
    Ok(base.join(suffix)?)
}

fn validate_policy(policy: &FaiRegistryPolicy) -> Result<()> {
    if policy.version != PRIVATE_REGISTRY_VERSION {
        bail!("unsupported private registry policy version");
    }
    validate_id(&policy.registry_id, "registry")?;
    for (id, key) in &policy.approved_signers {
        validate_id(id, "signer")?;
        decode_fixed::<32>(key)?;
    }
    Ok(())
}

fn authority_difference(
    old: &crate::FaiAuthoritySummary,
    new: &crate::FaiAuthoritySummary,
) -> crate::FaiAuthoritySummary {
    crate::FaiAuthoritySummary {
        tools: new
            .tools
            .iter()
            .filter(|value| !old.tools.contains(value))
            .cloned()
            .collect(),
        network_domains: new
            .network_domains
            .iter()
            .filter(|value| !old.network_domains.contains(value))
            .cloned()
            .collect(),
        secret_handles: new
            .secret_handles
            .iter()
            .filter(|value| !old.secret_handles.contains(value))
            .cloned()
            .collect(),
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

fn authority_is_empty(authority: &crate::FaiAuthoritySummary) -> bool {
    authority.tools.is_empty()
        && authority.network_domains.is_empty()
        && authority.secret_handles.is_empty()
        && authority.event_sources.is_empty()
        && authority.agent_triggers == 0
        && authority.workflow_nodes == 0
        && authority.routines == 0
        && authority.connector_runtimes == 0
}

fn validate_id(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 80
        || !value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
    {
        bail!("invalid private registry {kind} id");
    }
    Ok(())
}

fn validate_version(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 40
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("invalid private registry package version");
    }
    Ok(())
}

fn decode_fixed<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("invalid hexadecimal value");
    }
    let mut bytes = [0_u8; N];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)?;
    }
    Ok(bytes)
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn write_new_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(|| anyhow!("path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("invalid file name"))?;
    let temp = parent.join(format!(".{name}.{:016x}.tmp", rand::random::<u64>()));
    let result = (|| -> Result<()> {
        write_new_private(&temp, bytes)?;
        fs::rename(&temp, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (PathBuf, FaiPrivateRegistry, crate::FaiBundle) {
        let root = std::env::temp_dir().join(format!(
            "focaldesk-private-registry-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let registry = FaiPrivateRegistry::open(&root, &[31_u8; 32]).unwrap();
        registry.initialize_policy("engineering").unwrap();
        let project = crate::FaiForgeProject::example("focus-kit", "Focus Kit", "release");
        let bundle = crate::build_fai_project(&project, &[33_u8; 32]).unwrap();
        registry
            .approve_signer(bundle.manifest.signer.clone())
            .unwrap();
        (root, registry, bundle)
    }

    #[test]
    fn signed_catalog_lock_and_revocation_are_fail_closed() {
        let (root, registry, bundle) = registry();
        registry.publish(&bundle).unwrap();
        let catalog = registry.catalog().unwrap();
        verify_registry_catalog(&catalog, &registry.signing_public_key_hex(), None).unwrap();
        let lock = resolve_catalog_lock(&catalog, "focus-kit", "0.1.0").unwrap();
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(registry.package("focus-kit", "0.1.0").unwrap(), bundle);

        registry
            .revoke("focus-kit", "0.1.0", "unsafe regression")
            .unwrap();
        assert!(registry.package("focus-kit", "0.1.0").is_err());
        let revoked_catalog = registry.catalog().unwrap();
        assert!(revoked_catalog.catalog.packages[0].revoked);
        assert!(resolve_catalog_lock(&revoked_catalog, "focus-kit", "0.1.0").is_err());
        assert!(
            verify_registry_catalog(
                &catalog,
                &registry.signing_public_key_hex(),
                Some(revoked_catalog.catalog.sequence)
            )
            .is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn catalog_tampering_and_unapproved_publish_are_rejected() {
        let (root, registry, _) = registry();
        let project = crate::FaiForgeProject::example("other-kit", "Other Kit", "intruder");
        let bundle = crate::build_fai_project(&project, &[35_u8; 32]).unwrap();
        assert!(registry.publish(&bundle).is_err());
        let mut catalog = registry.catalog().unwrap();
        catalog.catalog.registry_id = "tampered".into();
        assert!(
            verify_registry_catalog(&catalog, &registry.signing_public_key_hex(), None).is_err()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn compatibility_report_flags_added_authority() {
        let (root, registry, first) = registry();
        registry.publish(&first).unwrap();
        let mut project = crate::FaiForgeProject::example("focus-kit", "Focus Kit", "release");
        project.manifest.version = "0.2.0".into();
        project.payload.agents[0]
            .tool_allowlist
            .push("list_windows".into());
        let second = crate::build_fai_project(&project, &[33_u8; 32]).unwrap();
        registry.publish(&second).unwrap();
        let catalog = registry.catalog().unwrap();
        let report = compare_catalog_versions(&catalog, "focus-kit", "0.1.0", "0.2.0").unwrap();
        assert!(report.requires_authority_review);
        assert_eq!(report.authority_added.tools, vec!["list_windows"]);
        fs::remove_dir_all(root).unwrap();
    }
}
