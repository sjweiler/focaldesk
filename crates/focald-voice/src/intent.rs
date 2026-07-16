//! The intent vocabulary. This enum is the *entire* surface area the LLM can
//! reach. Its JSON schema is sent to Ollama as a decoding constraint, and the
//! same type is used to deserialize the response — so the model literally
//! cannot produce anything your code doesn't already have a variant for.

use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "intent", rename_all = "snake_case", deny_unknown_fields)]
pub enum VoiceIntent {
    /// "open firefox", "launch a terminal on output 2"
    OpenApp { app: String, output: Option<u32> },

    /// "go to workspace 3", "switch to workspace five"
    FocusWorkspace { workspace: u32 },

    /// "move this window to output 1"
    MoveWindowToOutput { output: u32 },

    /// "move the window left"
    MoveWindow { direction: Direction },

    /// "close this window"
    CloseWindow,

    /// "set volume to 40 percent"
    SetVolume { percent: u8 },

    /// Escape hatch: the model routes anything it can't map here instead of
    /// guessing. `raw` carries the original utterance for logging/clarify.
    Unknown { raw: String },
}
