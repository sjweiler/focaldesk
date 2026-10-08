use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FaiForgeManifest {
    pub package_version: u16,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub dependencies: Vec<crate::FaiPackageDependency>,
    pub signer_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FaiForgeProject {
    pub manifest: FaiForgeManifest,
    pub payload: crate::FaiPackagePayload,
    pub scenarios: Vec<crate::ScenarioFixture>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiForgeReport {
    pub package_id: String,
    pub version: String,
    pub passed: bool,
    pub scenarios: Vec<crate::ScenarioReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FaiRegistryEntry {
    pub package_id: String,
    pub name: String,
    pub version: String,
    pub signer_id: String,
    pub digest_sha256: String,
}

pub struct FaiLocalRegistry {
    root: PathBuf,
}

impl FaiLocalRegistry {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        if root.exists() {
            let metadata = fs::symlink_metadata(&root)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!("AIOS registry must be a regular directory");
            }
        }
        Ok(Self { root })
    }

    pub fn add(&self, bundle: &crate::FaiBundle, overwrite: bool) -> Result<FaiRegistryEntry> {
        validate_registry_version(&bundle.manifest.version)?;
        let inspection = crate::FaiPackageManager::memory().inspect(bundle)?;
        if !inspection.signature_valid || !inspection.scenarios_passed {
            bail!("local registry accepts only valid signed packages with passing scenarios");
        }
        for dependency in &bundle.manifest.dependencies {
            if !self
                .bundle_path(&dependency.package_id, &dependency.version)?
                .exists()
            {
                bail!(
                    "local registry is missing dependency {} {}",
                    dependency.package_id,
                    dependency.version
                );
            }
        }
        let path = self.bundle_path(&bundle.manifest.id, &bundle.manifest.version)?;
        if path.exists() && !overwrite {
            bail!("package version already exists in the local registry");
        }
        let parent = path
            .parent()
            .ok_or_else(|| anyhow!("registry package path has no parent"))?;
        fs::create_dir_all(parent)?;
        fs::set_permissions(&self.root, fs::Permissions::from_mode(0o700))?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        let bytes = serde_json::to_vec_pretty(bundle)?;
        write_registry_file(&path, &bytes, overwrite)?;
        Ok(FaiRegistryEntry {
            package_id: bundle.manifest.id.clone(),
            name: bundle.manifest.name.clone(),
            version: bundle.manifest.version.clone(),
            signer_id: bundle.manifest.signer.id.clone(),
            digest_sha256: inspection.digest_sha256,
        })
    }

    pub fn list(&self, query: Option<&str>) -> Result<Vec<FaiRegistryEntry>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let query = query.map(str::to_ascii_lowercase);
        let mut entries = Vec::new();
        for package_dir in fs::read_dir(&self.root)? {
            let package_dir = package_dir?;
            if package_dir.file_type()?.is_symlink() || !package_dir.file_type()?.is_dir() {
                continue;
            }
            for file in fs::read_dir(package_dir.path())? {
                let file = file?;
                if file.file_type()?.is_symlink()
                    || !file.file_type()?.is_file()
                    || file.path().extension().and_then(|value| value.to_str()) != Some("fai")
                    || file.metadata()?.len() > 2 * 1024 * 1024
                {
                    continue;
                }
                let bundle: crate::FaiBundle = serde_json::from_slice(&fs::read(file.path())?)
                    .with_context(|| {
                        format!("decode registry package {}", file.path().display())
                    })?;
                let inspection = crate::FaiPackageManager::memory().inspect(&bundle)?;
                if !inspection.signature_valid {
                    bail!("local registry contains a package with an invalid signature");
                }
                let entry = FaiRegistryEntry {
                    package_id: bundle.manifest.id.clone(),
                    name: bundle.manifest.name,
                    version: bundle.manifest.version,
                    signer_id: bundle.manifest.signer.id,
                    digest_sha256: inspection.digest_sha256,
                };
                if query.as_ref().is_none_or(|query| {
                    entry.package_id.to_ascii_lowercase().contains(query)
                        || entry.name.to_ascii_lowercase().contains(query)
                }) {
                    entries.push(entry);
                }
            }
        }
        entries.sort_by(|left, right| {
            left.package_id
                .cmp(&right.package_id)
                .then(left.version.cmp(&right.version))
        });
        Ok(entries)
    }

    fn bundle_path(&self, package_id: &str, version: &str) -> Result<PathBuf> {
        crate::fai_signer_secret_handle(package_id)?;
        validate_registry_version(version)?;
        Ok(self.root.join(package_id).join(format!("{version}.fai")))
    }
}

impl FaiForgeProject {
    pub fn example(
        id: impl Into<String>,
        name: impl Into<String>,
        signer_id: impl Into<String>,
    ) -> Self {
        let id = id.into();
        Self {
            manifest: FaiForgeManifest {
                package_version: crate::FAI_PACKAGE_VERSION,
                id: id.clone(),
                name: name.into(),
                version: "0.1.0".into(),
                description: "A locally authored FocalDesk AIOS package.".into(),
                dependencies: Vec::new(),
                signer_id: signer_id.into(),
            },
            payload: crate::FaiPackagePayload {
                agents: vec![crate::AgentDefinition {
                    manifest_version: crate::AGENT_MANIFEST_VERSION,
                    id: format!("{id}-agent"),
                    name: "Example Agent".into(),
                    description: "A bounded read-only example agent.".into(),
                    instructions: "Inspect session status and return a concise summary.".into(),
                    tool_allowlist: vec!["get_session_status".into()],
                    max_tool_steps: 2,
                    max_context_chars: 8_000,
                    max_output_tokens: 256,
                    timeout_seconds: 60,
                    memory: false,
                    voice: false,
                    triggers: Vec::new(),
                    daily_token_limit: Some(10_000),
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
                name: "read-only-plan".into(),
                description: "The example agent uses only declared read-only authority.".into(),
                synthetic: true,
                initial: crate::ScenarioInitialState {
                    agent_tools: [(format!("{id}-agent"), vec!["get_session_status".into()])]
                        .into_iter()
                        .collect(),
                    ..Default::default()
                },
                steps: vec![crate::ScenarioStep::AgentPlan {
                    agent_id: format!("{id}-agent"),
                    tools: vec!["get_session_status".into()],
                    proposed_mutation: false,
                    confirmation_present: false,
                    lease: crate::ScenarioLeaseState::Active,
                    estimated_tokens: 256,
                }],
                expected_violation_codes: Vec::new(),
            }],
        }
    }

    pub fn test(&self) -> Result<FaiForgeReport> {
        // Materializing with a deterministic throwaway key exercises the exact
        // package schema and authority limits without persisting key material.
        let _ = materialize_fai_project(self, &[1_u8; 32])?;
        let scenarios = self
            .scenarios
            .iter()
            .cloned()
            .map(crate::evaluate_scenario)
            .collect::<Result<Vec<_>>>()?;
        Ok(FaiForgeReport {
            package_id: self.manifest.id.clone(),
            version: self.manifest.version.clone(),
            passed: scenarios.iter().all(|report| report.passed),
            scenarios,
        })
    }
}

fn validate_registry_version(version: &str) -> Result<()> {
    if version.is_empty()
        || version.len() > 40
        || !version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        bail!("registry package version is invalid");
    }
    Ok(())
}

fn write_registry_file(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("registry path has no parent"))?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow!("registry file name is invalid"))?;
    let temp = parent.join(format!(".{name}.{:016x}.tmp", rand::random::<u64>()));
    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        if overwrite {
            fs::rename(&temp, path)?;
        } else {
            fs::hard_link(&temp, path).context("publish new registry package without overwrite")?;
            fs::remove_file(&temp)?;
        }
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temp);
    }
    result
}

pub fn build_fai_project(
    project: &FaiForgeProject,
    secret_key: &[u8; 32],
) -> Result<crate::FaiBundle> {
    let bundle = materialize_fai_project(project, secret_key)?;
    let report = project.test_scenarios()?;
    if !report.iter().all(|scenario| scenario.passed) {
        bail!("AIOS package project failed its Scenario Lab suite");
    }
    Ok(bundle)
}

fn materialize_fai_project(
    project: &FaiForgeProject,
    secret_key: &[u8; 32],
) -> Result<crate::FaiBundle> {
    let mut bundle = crate::FaiBundle {
        manifest: crate::FaiPackageManifest {
            package_version: project.manifest.package_version,
            id: project.manifest.id.clone(),
            name: project.manifest.name.clone(),
            version: project.manifest.version.clone(),
            description: project.manifest.description.clone(),
            dependencies: project.manifest.dependencies.clone(),
            signer: crate::FaiSigner {
                id: project.manifest.signer_id.clone(),
                public_key_hex: "00".repeat(32),
            },
        },
        payload: project.payload.clone(),
        scenarios: project.scenarios.clone(),
        signature_hex: String::new(),
    };
    crate::sign_fai_bundle(&mut bundle, secret_key)?;
    Ok(bundle)
}

impl FaiForgeProject {
    fn test_scenarios(&self) -> Result<Vec<crate::ScenarioReport>> {
        self.scenarios
            .iter()
            .cloned()
            .map(crate::evaluate_scenario)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_is_reproducible_and_passes_scenarios() {
        let project = FaiForgeProject::example("focus-kit", "Focus Kit", "local-dev");
        assert!(project.test().unwrap().passed);
        let first = build_fai_project(&project, &[21_u8; 32]).unwrap();
        let second = build_fai_project(&project, &[21_u8; 32]).unwrap();
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&second).unwrap()
        );
    }

    #[test]
    fn local_registry_filters_and_rejects_missing_dependencies() {
        let root = std::env::temp_dir().join(format!(
            "focaldesk-forge-registry-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let registry = FaiLocalRegistry::open(&root).unwrap();
        let project = FaiForgeProject::example("focus-kit", "Focus Kit", "local-dev");
        let bundle = build_fai_project(&project, &[23_u8; 32]).unwrap();
        registry.add(&bundle, false).unwrap();
        assert_eq!(registry.list(Some("focus")).unwrap().len(), 1);
        assert!(registry.list(Some("missing")).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn build_refuses_a_project_with_a_failing_scenario() {
        let mut project = FaiForgeProject::example("focus-kit", "Focus Kit", "local-dev");
        project.scenarios[0].steps = vec![crate::ScenarioStep::AgentPlan {
            agent_id: "focus-kit-agent".into(),
            tools: vec!["get_session_status".into()],
            proposed_mutation: true,
            confirmation_present: false,
            lease: crate::ScenarioLeaseState::Active,
            estimated_tokens: 256,
        }];
        assert!(!project.test().unwrap().passed);
        assert!(build_fai_project(&project, &[25_u8; 32]).is_err());
    }
}
