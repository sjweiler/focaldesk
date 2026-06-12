#[derive(Debug)]
pub enum PermissionError {
    Store(String),
    InvalidRequest(String),
    PromptFailed(String),
}
