pub type Result<T> = std::result::Result<T, LaunchError>;

#[derive(Debug)]
pub enum LaunchError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Protocol(String),
    DaemonUnavailable,
    LaunchFailed(String),
}

impl From<std::io::Error> for LaunchError {
    fn from(err: std::io::Error) -> Self {
        LaunchError::Io(err)
    }
}

impl From<serde_json::Error> for LaunchError {
    fn from(err: serde_json::Error) -> Self {
        LaunchError::Json(err)
    }
}

impl From<String> for LaunchError {
    fn from(err: String) -> Self {
        LaunchError::Protocol(err)
    }
}
