use serde::{Deserialize, Serialize};

/// One installable package reported by the update backend.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdatePackage {
    /// Stable id used for install selection (`name` or PackageKit package-id).
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub arch: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

impl UpdatePackage {
    pub fn display_title(&self) -> String {
        if self.version.is_empty() {
            self.name.clone()
        } else {
            format!("{} {}", self.name, self.version)
        }
    }

    pub fn detail_text(&self) -> Option<String> {
        self.description
            .as_deref()
            .or(self.summary.as_deref())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(ToOwned::to_owned)
    }
}

/// Cached daemon snapshot. Cheap to clone onto the compositor thread.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateSnapshot {
    #[serde(default)]
    pub packages: Vec<UpdatePackage>,
    #[serde(default)]
    pub last_check_unix: Option<u64>,
    #[serde(default)]
    pub checking: bool,
    #[serde(default)]
    pub installing: bool,
    #[serde(default)]
    pub progress: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub backend: Option<String>,
}

impl UpdateSnapshot {
    pub fn available_count(&self) -> usize {
        self.packages.len()
    }

    pub fn has_updates(&self) -> bool {
        !self.packages.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detail_prefers_description_then_summary() {
        let mut package = UpdatePackage {
            id: "foo".into(),
            name: "foo".into(),
            version: "1".into(),
            arch: "x86_64".into(),
            repo: "updates".into(),
            summary: Some("short".into()),
            description: Some("longer text".into()),
        };
        assert_eq!(package.detail_text().as_deref(), Some("longer text"));
        package.description = None;
        assert_eq!(package.detail_text().as_deref(), Some("short"));
        package.summary = Some("  ".into());
        assert_eq!(package.detail_text(), None);
    }
}
