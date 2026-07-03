use std::path::PathBuf;

pub const SOCKET_BASENAME: &str = "focal-launchd.sock";
pub const DEFAULT_TIMEOUT_MS: u64 = 3000;

pub fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(SOCKET_BASENAME)
}
