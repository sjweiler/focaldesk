#[derive(Debug, Clone)]
pub enum LoginPhase {
    EnteringUsername,
    EnteringResponse {
        secret: bool,
        prompt: String,
    },
    Authenticating,
    /// Escape was pressed mid-login; `CancelSession` is in flight and we're
    /// waiting for greetd to ack it before starting a fresh attempt.
    Cancelling,
    Failed {
        message: String,
    },
    Starting,
}

pub struct LoginScreenState {
    pub username: String,
    pub input: String,
    pub phase: LoginPhase,
}

impl LoginScreenState {
    pub fn new() -> Self {
        Self {
            username: String::new(),
            input: String::new(),
            phase: LoginPhase::EnteringUsername,
        }
    }

    pub fn push_char(&mut self, ch: char) {
        self.input.push(ch);
    }

    pub fn backspace(&mut self) {
        self.input.pop();
    }

    pub fn clear_input(&mut self) {
        self.input.clear();
    }

    /// True while a prompt is on screen and further keystrokes should be masked
    /// (the "secret" `AuthMessageType` greetd sent for the current prompt).
    pub fn is_secret_input(&self) -> bool {
        matches!(
            self.phase,
            LoginPhase::EnteringResponse { secret: true, .. }
        )
    }

    pub fn prompt_text(&self) -> &str {
        match &self.phase {
            LoginPhase::EnteringUsername => "login:",
            LoginPhase::EnteringResponse { prompt, .. } => prompt.as_str(),
            LoginPhase::Authenticating => "authenticating...",
            LoginPhase::Cancelling => "cancelling...",
            LoginPhase::Failed { message } => message.as_str(),
            LoginPhase::Starting => "starting session...",
        }
    }

    /// Reset to a fresh attempt. Called after a failed login — greetd's session
    /// is dead at that point, so the next attempt must start over from
    /// CreateSession rather than resuming the old one.
    pub fn reset_for_retry(&mut self) {
        self.username.clear();
        self.input.clear();
        self.phase = LoginPhase::EnteringUsername;
    }
}

impl Default for LoginScreenState {
    fn default() -> Self {
        Self::new()
    }
}
