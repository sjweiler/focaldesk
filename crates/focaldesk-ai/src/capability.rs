use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CapabilityPolicy {
    #[serde(default)]
    pub filesystem_roots: Option<Vec<PathBuf>>,
    #[serde(default)]
    pub network_origins: Option<Vec<String>>,
    #[serde(default)]
    pub applications: Option<Vec<String>>,
    #[serde(default)]
    pub workspaces: Option<Vec<String>>,
    #[serde(default)]
    pub services: Option<Vec<String>>,
    #[serde(default)]
    pub secret_handles: Option<Vec<String>>,
    #[serde(default)]
    pub context_kinds: Option<Vec<crate::ContextKind>>,
    #[serde(default = "default_lease_seconds")]
    pub lease_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityLease {
    pub lease_id: String,
    pub run_id: String,
    pub agent_id: String,
    pub tools: Vec<String>,
    pub policy: CapabilityPolicy,
    pub issued_at_unix: u64,
    pub expires_at_unix: u64,
    pub revoked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityPreview {
    pub agent_id: String,
    pub tools: Vec<String>,
    pub policy: CapabilityPolicy,
    pub lease_seconds: u64,
}

const fn default_lease_seconds() -> u64 {
    120
}

impl Default for CapabilityPolicy {
    fn default() -> Self {
        Self {
            filesystem_roots: None,
            network_origins: None,
            applications: None,
            workspaces: None,
            services: None,
            secret_handles: None,
            context_kinds: None,
            lease_seconds: default_lease_seconds(),
        }
    }
}

impl CapabilityPolicy {
    pub fn validate(&self) -> Result<()> {
        if !(10..=3_600).contains(&self.lease_seconds) {
            bail!("capability lease_seconds must be between 10 and 3600");
        }
        validate_list(&self.applications, "application")?;
        validate_list(&self.workspaces, "workspace")?;
        validate_list(&self.services, "service")?;
        validate_list(&self.secret_handles, "secret handle")?;
        if let Some(kinds) = &self.context_kinds
            && (kinds.len() > 6 || kinds.iter().collect::<BTreeSet<_>>().len() != kinds.len())
        {
            bail!("context capability contains invalid or duplicate kinds");
        }
        if let Some(roots) = &self.filesystem_roots
            && (roots.len() > 32
                || roots.iter().any(|root| {
                    !root.is_absolute()
                        || root
                            .components()
                            .any(|part| matches!(part, Component::ParentDir))
                }))
        {
            bail!("filesystem capability roots must be 0-32 absolute normalized paths");
        }
        if let Some(origins) = &self.network_origins {
            if origins.len() > 32 {
                bail!("network capability may contain at most 32 origins");
            }
            for origin in origins {
                let parsed = url::Url::parse(origin)?;
                if !matches!(parsed.scheme(), "http" | "https")
                    || parsed.host_str().is_none()
                    || parsed.path() != "/"
                    || parsed.query().is_some()
                    || parsed.fragment().is_some()
                {
                    bail!("network capabilities must be HTTP(S) origins without paths");
                }
            }
        }
        Ok(())
    }

    pub fn intersect(&self, ceiling: &Self) -> Self {
        Self {
            filesystem_roots: intersect_paths(&self.filesystem_roots, &ceiling.filesystem_roots),
            network_origins: intersect_values(&self.network_origins, &ceiling.network_origins),
            applications: intersect_values(&self.applications, &ceiling.applications),
            workspaces: intersect_values(&self.workspaces, &ceiling.workspaces),
            services: intersect_values(&self.services, &ceiling.services),
            secret_handles: intersect_values(&self.secret_handles, &ceiling.secret_handles),
            context_kinds: intersect_context_kinds(&self.context_kinds, &ceiling.context_kinds),
            lease_seconds: self.lease_seconds.min(ceiling.lease_seconds),
        }
    }

    pub fn authorize(&self, tool: &str, arguments: &Value) -> Result<()> {
        walk_arguments(arguments, &mut |key, value| match key {
            "path" | "source" | "destination" => {
                if let Some(path) = value.as_str() {
                    authorize_path(self.filesystem_roots.as_deref(), Path::new(path))?;
                }
                Ok(())
            }
            "url" | "origin" => {
                if let Some(url) = value.as_str() {
                    authorize_origin(self.network_origins.as_deref(), url)?;
                }
                Ok(())
            }
            "application" | "application_id" | "app_id" => {
                authorize_value(self.applications.as_deref(), value, "application")
            }
            "workspace" | "workspace_id" => {
                authorize_value(self.workspaces.as_deref(), value, "workspace")
            }
            "service" | "service_name" => {
                authorize_value(self.services.as_deref(), value, "service")
            }
            "secret_handle" => {
                authorize_value(self.secret_handles.as_deref(), value, "secret handle")
            }
            _ => Ok(()),
        })
        .map_err(|error| anyhow::anyhow!("capability denied tool {tool}: {error}"))
    }
}

fn intersect_context_kinds(
    left: &Option<Vec<crate::ContextKind>>,
    right: &Option<Vec<crate::ContextKind>>,
) -> Option<Vec<crate::ContextKind>> {
    match (left, right) {
        (None, value) | (value, None) => value.clone(),
        (Some(left), Some(right)) => Some(
            left.iter()
                .filter(|value| right.contains(value))
                .copied()
                .collect(),
        ),
    }
}

fn validate_list(values: &Option<Vec<String>>, kind: &str) -> Result<()> {
    if let Some(values) = values
        && (values.len() > 64
            || values
                .iter()
                .any(|value| value.trim().is_empty() || value.len() > 200)
            || values.iter().collect::<BTreeSet<_>>().len() != values.len())
    {
        bail!("{kind} capability contains invalid or duplicate values");
    }
    Ok(())
}

fn intersect_values(
    left: &Option<Vec<String>>,
    right: &Option<Vec<String>>,
) -> Option<Vec<String>> {
    match (left, right) {
        (None, value) | (value, None) => value.clone(),
        (Some(left), Some(right)) => Some(
            left.iter()
                .filter(|value| right.contains(value))
                .cloned()
                .collect(),
        ),
    }
}

fn intersect_paths(
    left: &Option<Vec<PathBuf>>,
    right: &Option<Vec<PathBuf>>,
) -> Option<Vec<PathBuf>> {
    match (left, right) {
        (None, value) | (value, None) => value.clone(),
        (Some(left), Some(right)) => {
            let mut narrowed = BTreeSet::new();
            for left_root in left {
                for right_root in right {
                    if left_root.starts_with(right_root) {
                        narrowed.insert(left_root.clone());
                    } else if right_root.starts_with(left_root) {
                        narrowed.insert(right_root.clone());
                    }
                }
            }
            Some(narrowed.into_iter().collect())
        }
    }
}

fn authorize_path(roots: Option<&[PathBuf]>, path: &Path) -> Result<()> {
    let Some(roots) = roots else {
        return Ok(());
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        bail!("path must be absolute and normalized");
    }
    if roots.iter().any(|root| path.starts_with(root)) {
        Ok(())
    } else {
        bail!("path is outside the leased filesystem roots")
    }
}

fn authorize_origin(origins: Option<&[String]>, value: &str) -> Result<()> {
    let Some(origins) = origins else {
        return Ok(());
    };
    let parsed = url::Url::parse(value)?;
    let origin = parsed.origin().ascii_serialization();
    if origins
        .iter()
        .any(|allowed| allowed.trim_end_matches('/') == origin)
    {
        Ok(())
    } else {
        bail!("network origin is not leased")
    }
}

fn authorize_value(allowed: Option<&[String]>, value: &Value, kind: &str) -> Result<()> {
    let Some(allowed) = allowed else {
        return Ok(());
    };
    let value = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("{kind} must be a string"))?;
    if allowed.iter().any(|item| item == value) {
        Ok(())
    } else {
        bail!("{kind} is not leased")
    }
}

fn walk_arguments(
    value: &Value,
    visitor: &mut impl FnMut(&str, &Value) -> Result<()>,
) -> Result<()> {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                visitor(key, value)?;
                walk_arguments(value, visitor)?;
            }
        }
        Value::Array(array) => {
            for value in array {
                walk_arguments(value, visitor)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn intersection_can_only_narrow_authority() {
        let broad = CapabilityPolicy {
            filesystem_roots: Some(vec![PathBuf::from("/home/person")]),
            applications: Some(vec!["editor".into(), "browser".into()]),
            lease_seconds: 120,
            ..Default::default()
        };
        let ceiling = CapabilityPolicy {
            filesystem_roots: Some(vec![PathBuf::from("/home/person/project")]),
            applications: Some(vec!["editor".into()]),
            lease_seconds: 30,
            ..Default::default()
        };
        let effective = broad.intersect(&ceiling);
        assert_eq!(effective.applications.unwrap(), vec!["editor"]);
        assert_eq!(effective.lease_seconds, 30);
        assert_eq!(
            effective.filesystem_roots.unwrap(),
            vec![PathBuf::from("/home/person/project")]
        );
    }

    #[test]
    fn scoped_arguments_fail_closed() {
        let policy = CapabilityPolicy {
            filesystem_roots: Some(vec![PathBuf::from("/safe")]),
            network_origins: Some(vec!["https://example.com".into()]),
            ..Default::default()
        };
        policy.validate().unwrap();
        assert!(
            policy
                .authorize("read", &json!({"path":"/safe/file"}))
                .is_ok()
        );
        assert!(
            policy
                .authorize("read", &json!({"path":"/etc/passwd"}))
                .is_err()
        );
        assert!(
            policy
                .authorize("fetch", &json!({"url":"https://evil.example/x"}))
                .is_err()
        );
    }
}
