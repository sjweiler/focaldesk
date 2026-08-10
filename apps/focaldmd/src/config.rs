//! focaldmd configuration: `/etc/focaldmd.toml`, overlaid on defaults.
//! Every field has a sane default so the daemon runs unconfigured on a
//! freshly installed system; the file only needs to override what differs.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Context as _;
use serde::Deserialize;

const CONFIG_PATH: &str = "/etc/focaldmd.toml";

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Unprivileged user the greeter runs as.
    pub greeter_user: String,
    /// Command exec'd to start the greeter.
    pub greeter_cmd: String,
    /// Command exec'd for the authenticated user's session (focaldesk).
    pub session_cmd: String,
    /// Extra environment variables exported to the authenticated user's
    /// session. This is the native-display-manager equivalent of variables
    /// set by a GDM session wrapper.
    pub session_environment: BTreeMap<String, String>,
    /// Unix socket the greeter connects to.
    pub socket_path: PathBuf,
    /// PAM service name (an entry under /etc/pam.d/) for authenticating a
    /// human and launching their session.
    pub pam_service: String,
    /// PAM service name for the greeter's own (non-interactive) session —
    /// distinct from `pam_service` because it must never require a password:
    /// auth/account are `pam_permit`, only the session stack (pam_systemd)
    /// matters, giving the greeter user a real seat.
    pub greeter_pam_service: String,
    /// TTY handed to PAM and the launched session (e.g. "tty1").
    pub tty_name: String,
    /// VT number, exported to the session as XDG_VTNR.
    pub vt: u32,
    /// XKB layout exported to the session as XKB_DEFAULT_LAYOUT (e.g. "us", "de").
    pub keyboard_layout: String,
    /// XKB variant exported as XKB_DEFAULT_VARIANT (e.g. "dvorak"). Empty for none.
    pub keyboard_variant: String,
    /// XKB model exported as XKB_DEFAULT_MODEL. Empty defers to xkbcommon's default.
    pub keyboard_model: String,
    /// XKB options exported as XKB_DEFAULT_OPTIONS (e.g. "ctrl:nocaps"). Empty for none.
    pub keyboard_options: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            greeter_user: "focaldm".into(),
            greeter_cmd: "/usr/libexec/focaldm-greeter".into(),
            session_cmd: "/usr/local/bin/focaldesk-desktop".into(),
            session_environment: BTreeMap::new(),
            socket_path: PathBuf::from("/run/focaldmd/greeter.sock"),
            pam_service: "focaldmd".into(),
            greeter_pam_service: "focaldmd-greeter".into(),
            tty_name: "tty1".into(),
            vt: 1,
            keyboard_layout: "us".into(),
            keyboard_variant: String::new(),
            keyboard_model: String::new(),
            keyboard_options: String::new(),
        }
    }
}

impl Config {
    /// Load `/etc/focaldmd.toml` if present, else fall back to defaults.
    pub fn load() -> anyhow::Result<Self> {
        match std::fs::read_to_string(CONFIG_PATH) {
            Ok(raw) => toml::from_str(&raw).context("parse /etc/focaldmd.toml"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e).context("read /etc/focaldmd.toml"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn parses_session_environment() {
        let cfg: Config = toml::from_str(
            r#"
            [session_environment]
            FOCALDESK_HDR_OUTPUT = "DP-3"
            FOCALDESK_HDR_RENDER = "1"
            "#,
        )
        .expect("parse focaldmd config");

        assert_eq!(
            cfg.session_environment.get("FOCALDESK_HDR_OUTPUT"),
            Some(&"DP-3".to_string())
        );
        assert_eq!(
            cfg.session_environment.get("FOCALDESK_HDR_RENDER"),
            Some(&"1".to_string())
        );
    }
}
