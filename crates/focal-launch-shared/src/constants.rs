use std::path::PathBuf;

pub const SOCKET_BASENAME: &str = "focal-launchd.sock";
pub const SOCKET_ENV: &str = "FOCAL_LAUNCHD_SOCKET";
pub const DEFAULT_TIMEOUT_MS: u64 = 3000;

pub fn socket_path() -> std::io::Result<PathBuf> {
    focaldesk_ipc::transport::socket_path(SOCKET_ENV, SOCKET_BASENAME)
        .map_err(std::io::Error::other)
}
