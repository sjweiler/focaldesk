use std::process::{Command, Output, Stdio};

use crate::model::UpdatePackage;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateBackendKind {
    PackageKit,
    Dnf5,
    Dnf,
}

impl UpdateBackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PackageKit => "packagekit",
            Self::Dnf5 => "dnf5",
            Self::Dnf => "dnf",
        }
    }
}

pub fn detect_backend() -> Option<UpdateBackendKind> {
    if command_exists("pkcon") {
        Some(UpdateBackendKind::PackageKit)
    } else if command_exists("dnf5") {
        Some(UpdateBackendKind::Dnf5)
    } else if command_exists("dnf") {
        Some(UpdateBackendKind::Dnf)
    } else {
        None
    }
}

fn command_exists(name: &str) -> bool {
    Command::new("which")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn list_updates(
    kind: UpdateBackendKind,
    refresh_metadata: bool,
) -> Result<Vec<UpdatePackage>, String> {
    match kind {
        UpdateBackendKind::PackageKit => list_packagekit(refresh_metadata),
        UpdateBackendKind::Dnf5 => list_dnf("dnf5", refresh_metadata),
        UpdateBackendKind::Dnf => list_dnf("dnf", refresh_metadata),
    }
}

pub fn install_updates(kind: UpdateBackendKind, ids: &[String]) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    match kind {
        UpdateBackendKind::PackageKit => install_packagekit(ids),
        UpdateBackendKind::Dnf5 => install_dnf("dnf5", ids),
        UpdateBackendKind::Dnf => install_dnf("dnf", ids),
    }
}

fn list_packagekit(refresh_metadata: bool) -> Result<Vec<UpdatePackage>, String> {
    if refresh_metadata {
        let _ = run("pkcon", &["refresh", "--noninteractive"]);
    }
    let output = run("pkcon", &["get-updates", "-p"])?;
    if !output.status.success() {
        return Err(format!(
            "pkcon get-updates failed: {}",
            stderr_or_status(&output)
        ));
    }
    let stdout = decode_output(&output);
    let packages = parse_pkcon_plain(&stdout);
    if !packages.is_empty() {
        return Ok(packages);
    }
    Ok(parse_pkcon_human(&stdout)?)
}

fn install_packagekit(ids: &[String]) -> Result<(), String> {
    let mut args = vec!["update", "--noninteractive"];
    args.extend(ids.iter().map(String::as_str));
    let output = run("pkcon", &args)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "pkcon update failed: {}",
            stderr_or_status(&output)
        ))
    }
}

fn list_dnf(bin: &str, refresh_metadata: bool) -> Result<Vec<UpdatePackage>, String> {
    if let Ok(packages) = list_dnf_repoquery(bin, refresh_metadata)
        && !packages.is_empty()
    {
        return Ok(packages);
    }
    list_dnf_check_update(bin, refresh_metadata)
}

fn list_dnf_repoquery(bin: &str, refresh_metadata: bool) -> Result<Vec<UpdatePackage>, String> {
    let format = if bin == "dnf5" {
        "%{name}\t%{evr}\t%{arch}\t%{from_repo}\t%{summary}\n"
    } else {
        "%{name}\t%{evr}\t%{arch}\t%{reponame}\t%{summary}"
    };
    let mut args = vec!["repoquery", "--upgrades"];
    if refresh_metadata {
        args.push("--refresh");
    }
    if bin == "dnf5" {
        args.push("--queryformat");
    } else {
        args.push("--qf");
    }
    args.push(format);
    let output = run(bin, &args)?;
    if !output.status.success() && !dnf_updates_exit(output.status.code()) {
        return Err(format!(
            "{bin} repoquery failed: {}",
            stderr_or_status(&output)
        ));
    }
    Ok(parse_dnf_repoquery(&decode_output(&output)))
}

fn list_dnf_check_update(bin: &str, refresh_metadata: bool) -> Result<Vec<UpdatePackage>, String> {
    let command = if bin == "dnf5" {
        "check-upgrade"
    } else {
        "check-update"
    };
    let mut args = vec![command];
    if refresh_metadata {
        args.push("--refresh");
    }
    let output = run(bin, &args)?;
    let code = output.status.code();
    if !output.status.success() && !dnf_updates_exit(code) {
        return Err(format!(
            "{bin} {command} failed: {}",
            stderr_or_status(&output)
        ));
    }
    Ok(parse_dnf_check_update(&decode_output(&output)))
}

fn install_dnf(bin: &str, ids: &[String]) -> Result<(), String> {
    let names: Vec<&str> = ids.iter().map(|id| package_name(id)).collect();
    let mut args = vec!["upgrade", "-y"];
    args.extend(names.iter().copied());
    let output = if command_exists("pkexec") {
        let mut pkexec = vec![bin];
        pkexec.extend(args.iter().copied());
        run("pkexec", &pkexec)?
    } else {
        run(bin, &args)?
    };
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{bin} upgrade failed: {}",
            stderr_or_status(&output)
        ))
    }
}

fn run(bin: &str, args: &[&str]) -> Result<Output, String> {
    Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|err| format!("failed to spawn {bin}: {err}"))
}

fn decode_output(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_or_status(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        format!("exit {}", output.status)
    } else {
        trimmed.to_string()
    }
}

fn dnf_updates_exit(code: Option<i32>) -> bool {
    matches!(code, Some(100))
}

fn package_name(id: &str) -> &str {
    id.split(';').next().unwrap_or(id)
}

pub fn parse_pkcon_plain(stdout: &str) -> Vec<UpdatePackage> {
    stdout.lines().filter_map(parse_pkcon_package_id).collect()
}

fn parse_pkcon_package_id(line: &str) -> Option<UpdatePackage> {
    let line = line.trim();
    if line.is_empty() || line.starts_with("Transaction") {
        return None;
    }
    let package_id = line
        .split_whitespace()
        .find(|token| token.contains(';'))
        .unwrap_or(line);
    let mut parts = package_id.split(';');
    let name = parts.next()?.trim();
    let version = parts.next().unwrap_or("").trim();
    let arch = parts.next().unwrap_or("").trim();
    let repo = parts.next().unwrap_or("").trim();
    if name.is_empty() || name.contains(' ') {
        return None;
    }
    Some(UpdatePackage {
        id: package_id.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        arch: arch.to_string(),
        repo: repo.to_string(),
        summary: None,
        description: None,
    })
}

pub fn parse_pkcon_human(stdout: &str) -> Result<Vec<UpdatePackage>, String> {
    let mut packages = Vec::new();
    let mut pending: Option<UpdatePackage> = None;
    for line in stdout.lines() {
        if let Some(package) = parse_pkcon_human_package(line) {
            if let Some(previous) = pending.take() {
                packages.push(previous);
            }
            pending = Some(package);
            continue;
        }
        if let Some(package) = pending.as_mut() {
            let trimmed = line.trim();
            if !trimmed.is_empty() && package.summary.is_none() {
                package.summary = Some(trimmed.to_string());
            }
        }
    }
    if let Some(previous) = pending.take() {
        packages.push(previous);
    }
    Ok(packages)
}

fn parse_pkcon_human_package(line: &str) -> Option<UpdatePackage> {
    let trimmed = line.trim();
    let rest = trimmed
        .strip_prefix("Installed")
        .or_else(|| trimmed.strip_prefix("Available"))
        .or_else(|| trimmed.strip_prefix("Important"))
        .or_else(|| trimmed.strip_prefix("Security"))
        .or_else(|| trimmed.strip_prefix("Bugfix"))
        .or_else(|| trimmed.strip_prefix("Enhancement"))
        .map(str::trim)?;
    let (nevra, repo) = split_repo_suffix(rest);
    parse_nevra(nevra).map(|(name, version, arch)| UpdatePackage {
        id: nevra.to_string(),
        name,
        version,
        arch,
        repo: repo.unwrap_or_default(),
        summary: None,
        description: None,
    })
}

pub fn parse_dnf_repoquery(stdout: &str) -> Vec<UpdatePackage> {
    stdout
        .lines()
        .filter_map(|line| {
            let columns: Vec<&str> = line.split('\t').collect();
            if columns.len() < 2 {
                return None;
            }
            let name = columns[0].trim();
            if name.is_empty() || name.contains(' ') {
                return None;
            }
            let version = columns.get(1).copied().unwrap_or("").trim();
            let arch = columns.get(2).copied().unwrap_or("").trim();
            let repo = columns.get(3).copied().unwrap_or("").trim();
            let summary = columns
                .get(4..)
                .map(|parts| parts.join("\t"))
                .map(|text| text.trim().to_string())
                .filter(|text| !text.is_empty());
            Some(UpdatePackage {
                id: name.to_string(),
                name: name.to_string(),
                version: version.to_string(),
                arch: arch.to_string(),
                repo: repo.to_string(),
                summary,
                description: None,
            })
        })
        .collect()
}

pub fn parse_dnf_check_update(stdout: &str) -> Vec<UpdatePackage> {
    stdout
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty()
                || line.starts_with("Last metadata")
                || line.starts_with("Fedora")
                || line.starts_with("Extra Packages")
                || line.starts_with("Security")
                || line.contains("B/s")
                || line.contains("---")
            {
                return None;
            }
            let columns: Vec<&str> = line.split_whitespace().collect();
            if columns.len() < 2 {
                return None;
            }
            let (name, arch) = split_name_arch(columns[0]);
            if name.is_empty() {
                return None;
            }
            Some(UpdatePackage {
                id: name.clone(),
                name,
                version: columns[1].to_string(),
                arch,
                repo: columns.get(2).copied().unwrap_or("").to_string(),
                summary: None,
                description: None,
            })
        })
        .collect()
}

fn split_name_arch(nevra_or_name: &str) -> (String, String) {
    if let Some((name, arch)) = nevra_or_name.rsplit_once('.')
        && !name.is_empty()
        && matches!(arch, "x86_64" | "i686" | "aarch64" | "noarch" | "src")
    {
        return (name.to_string(), arch.to_string());
    }
    (nevra_or_name.to_string(), String::new())
}

fn split_repo_suffix(text: &str) -> (&str, Option<String>) {
    if let Some(start) = text.rfind('(')
        && text.ends_with(')')
    {
        let nevra = text[..start].trim();
        let repo = text[start + 1..text.len() - 1].trim();
        return (nevra, Some(repo.to_string()));
    }
    (text.trim(), None)
}

fn parse_nevra(nevra: &str) -> Option<(String, String, String)> {
    let (name_version, arch) = nevra.rsplit_once('.')?;
    let (name, version) = name_version.rsplit_once('-').and_then(|(rest, release)| {
        rest.rsplit_once('-')
            .map(|(name, version)| (name, format!("{version}-{release}")))
            .or_else(|| Some((rest, release.to_string())))
    })?;
    if name.is_empty() {
        return None;
    }
    Some((name.to_string(), version, arch.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pkcon_plain_package_ids() {
        let packages = parse_pkcon_plain(
            "firefox;142.0-1.fc43;x86_64;updates\nkernel;6.16.3-200.fc43;x86_64;updates\n",
        );
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "firefox");
        assert_eq!(packages[0].version, "142.0-1.fc43");
        assert_eq!(packages[1].id, "kernel;6.16.3-200.fc43;x86_64;updates");
    }

    #[test]
    fn parses_pkcon_human_summaries() {
        let packages = parse_pkcon_human(
            "Available     firefox-142.0-1.fc43.x86_64 (updates)\n              Web browser\nSecurity      kernel-6.16.3-200.fc43.x86_64 (updates)\n",
        )
        .unwrap();
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "firefox");
        assert_eq!(packages[0].summary.as_deref(), Some("Web browser"));
        assert_eq!(packages[1].name, "kernel");
    }

    #[test]
    fn parses_dnf_repoquery_with_summary() {
        let packages = parse_dnf_repoquery(
            "firefox\t142.0-1.fc43\tx86_64\tupdates\tWeb browser\nmesa-libGL\t25.1.0-1.fc43\tx86_64\tupdates\tMesa libGL runtime\n",
        );
        assert_eq!(packages[0].summary.as_deref(), Some("Web browser"));
        assert_eq!(packages[1].name, "mesa-libGL");
    }

    #[test]
    fn parses_dnf_check_update_lines() {
        let packages = parse_dnf_check_update(
            "Last metadata expiration check: 0:12:34 ago\nfirefox.x86_64                 142.0-1.fc43            updates\nkernel.x86_64                  6.16.3-200.fc43         updates\n",
        );
        assert_eq!(packages.len(), 2);
        assert_eq!(packages[0].name, "firefox");
        assert_eq!(packages[0].arch, "x86_64");
        assert_eq!(packages[1].version, "6.16.3-200.fc43");
    }
}
