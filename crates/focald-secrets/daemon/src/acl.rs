//! Per-client access control for the native IPC surface.
//!
//! Peer identity, in order of preference:
//!   1. `unit:<name>`  — the systemd user unit owning the peer PID
//!      (asked of org.freedesktop.systemd1 on the session bus; unforgeable for
//!      user services)
//!   2. `exe:<path>`   — readlink(/proc/<pid>/exe) fallback for non-unit peers
//!
//! Grants live in a TOML file and are matched with simple `*` globs.
//! The file's mtime is checked on every request (a stat is ~1µs), so edits
//! take effect immediately without inotify machinery or a restart.
//!
//! Default posture: deny. An absent config file means nothing is granted.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Deserialize, Default, Clone)]
pub struct AclConfig {
    #[serde(default)]
    pub grants: HashMap<String, Grant>,
}

#[derive(Deserialize, Default, Clone)]
pub struct Grant {
    #[serde(default)]
    pub allow: Vec<String>,
    /// Optional: also allow creating/overwriting keys matching these globs.
    /// If omitted, `allow` covers read and write both.
    #[serde(default)]
    pub allow_write: Option<Vec<String>>,
}

pub struct Acl {
    path: PathBuf,
    loaded: AclConfig,
    mtime: Option<SystemTime>,
}

impl Acl {
    pub fn new(path: PathBuf) -> Self {
        let mut a = Acl {
            path,
            loaded: AclConfig::default(),
            mtime: None,
        };
        a.reload_if_changed();
        a
    }

    pub fn reload_if_changed(&mut self) {
        let meta = std::fs::metadata(&self.path).ok();
        let mtime = meta.and_then(|m| m.modified().ok());
        if mtime == self.mtime && self.mtime.is_some() {
            return;
        }
        self.mtime = mtime;
        self.loaded = match std::fs::read_to_string(&self.path) {
            Ok(s) => match toml::from_str(&s) {
                Ok(c) => {
                    log::info!(
                        "acl: loaded {} grant(s) from {}",
                        count(&c),
                        self.path.display()
                    );
                    c
                }
                Err(e) => {
                    log::error!(
                        "acl: {} parse error: {e}; keeping previous grants",
                        self.path.display()
                    );
                    self.loaded.clone()
                }
            },
            Err(_) => {
                log::warn!(
                    "acl: {} not readable; default-deny in effect",
                    self.path.display()
                );
                AclConfig::default()
            }
        };

        fn count(c: &AclConfig) -> usize {
            c.grants.len()
        }

        // Grants keyed to interpreter binaries match *every* script that
        // interpreter ever runs — that's a hole, not a grant. Warn loudly.
        const INTERPRETERS: &[&str] = &[
            "python", "perl", "ruby", "node", "deno", "bun", "bash", "sh", "dash", "zsh", "fish",
        ];
        for identity in self.loaded.grants.keys() {
            if let Some(path) = identity.strip_prefix("exe:") {
                let base = path.rsplit('/').next().unwrap_or(path);
                let base = base.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
                if INTERPRETERS.contains(&base) {
                    log::warn!(
                        "acl: grant for {identity} matches ANY {base} script run by this user; \
                         use unit: identities for real deployments"
                    );
                }
            }
        }
    }

    pub fn check(&self, identity: &str, key: &str, write: bool) -> bool {
        let Some(grant) = self.loaded.grants.get(identity) else {
            return false;
        };
        let read_ok = grant.allow.iter().any(|p| glob_match(p, key));
        if !write {
            return read_ok;
        }
        match &grant.allow_write {
            Some(w) => w.iter().any(|p| glob_match(p, key)),
            None => read_ok,
        }
    }

    /// Keys visible to `identity` for `list` responses.
    #[allow(dead_code)]
    pub fn filter_readable<'a>(
        &self,
        identity: &str,
        keys: impl Iterator<Item = &'a str>,
    ) -> Vec<String> {
        keys.filter(|k| self.check(identity, k, false))
            .map(|k| k.to_string())
            .collect()
    }
}

/// Minimal `*` glob (matches any run of characters, including `/`).
pub fn glob_match(pattern: &str, s: &str) -> bool {
    fn inner(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => inner(&p[1..], s) || (!s.is_empty() && inner(p, &s[1..])),
            (Some(pc), Some(sc)) if pc == sc => inner(&p[1..], &s[1..]),
            _ => false,
        }
    }
    inner(pattern.as_bytes(), s.as_bytes())
}

/// systemd user-manager availability: 0 unknown, 1 available, 2 unavailable.
static SYSTEMD_STATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Resolve a peer PID to an identity string.
pub async fn identify_peer(conn: Option<&zbus::Connection>, pid: i32) -> String {
    use std::sync::atomic::Ordering;
    if let Some(conn) = conn {
        if SYSTEMD_STATE.load(Ordering::Relaxed) != 2 {
            match unit_for_pid(conn, pid).await {
                Ok(unit) => {
                    SYSTEMD_STATE.store(1, Ordering::Relaxed);
                    return format!("unit:{unit}");
                }
                Err(zbus::Error::MethodError(name, _, _))
                    if name.as_str().ends_with("NoUnitForPID") =>
                {
                    // systemd is present; this pid just isn't in a unit.
                    SYSTEMD_STATE.store(1, Ordering::Relaxed);
                }
                Err(e) => {
                    if SYSTEMD_STATE.swap(2, Ordering::Relaxed) != 2 {
                        log::info!(
                            "acl: systemd user manager unavailable ({e}); \
                             falling back to exe-path identities"
                        );
                    }
                }
            }
        }
    }
    match std::fs::read_link(format!("/proc/{pid}/exe")) {
        Ok(p) => format!("exe:{}", p.display()),
        Err(_) => format!("pid:{pid}"),
    }
}

async fn unit_for_pid(conn: &zbus::Connection, pid: i32) -> Result<String, zbus::Error> {
    // org.freedesktop.systemd1.Manager.GetUnitByPID(u) -> o
    let reply = conn
        .call_method(
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            Some("org.freedesktop.systemd1.Manager"),
            "GetUnitByPID",
            &(pid as u32),
        )
        .await?;
    let path: zbus::zvariant::OwnedObjectPath = reply.body().deserialize()?;
    // Resolve the unit path to its Id property (human-readable unit name).
    let id: String = conn
        .call_method(
            Some("org.freedesktop.systemd1"),
            path.as_str(),
            Some("org.freedesktop.DBus.Properties"),
            "Get",
            &("org.freedesktop.systemd1.Unit", "Id"),
        )
        .await?
        .body()
        .deserialize::<zbus::zvariant::OwnedValue>()
        .and_then(|v| String::try_from(v).map_err(Into::into))?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::glob_match;
    #[test]
    fn globs() {
        assert!(glob_match("google/*", "google/oauth-refresh"));
        assert!(glob_match("*", "anything/at/all"));
        assert!(glob_match("a*c", "abc"));
        assert!(!glob_match("google/*", "microsoft/token"));
        assert!(!glob_match("", "x"));
        assert!(glob_match("", ""));
    }
}
