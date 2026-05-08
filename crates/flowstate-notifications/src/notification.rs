#[derive(Clone, Debug)]
pub struct Notification {
    pub id: u64,
    pub title: String,
    pub body: String,
    pub timeout_ms: Option<u64>,
}
