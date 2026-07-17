//! Ollama intent extraction. The `format` field carries the JSON schema for
//! `VoiceIntent`, which Ollama compiles into a sampling grammar — output that
//! violates the schema is impossible at the token level, not just discouraged.
//!
//! Even so, treat the response as untrusted text: the only thing that promotes
//! it to a value is `serde_json::from_str::<VoiceIntent>()` in `extract_intent`.

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::intent::VoiceIntent;

const OLLAMA_URL: &str = "http://127.0.0.1:11434/api/chat";
const MODEL: &str = "qwen2.5:7b";

/// Live context injected into the system prompt so the model can ground
/// fuzzy references ("the big monitor", "my browser") in real names/IDs.
pub struct PromptContext {
    /// e.g. [(0, "DP-1"), (1, "DP-2"), (2, "HDMI-A-1")]
    pub outputs: Vec<(u32, String)>,
    /// App names/aliases the resolver knows about, e.g. ["firefox", "browser", "kitty", "terminal"]
    pub known_apps: Vec<String>,
}

fn system_prompt(ctx: &PromptContext) -> String {
    let outputs = ctx
        .outputs
        .iter()
        .map(|(id, name)| format!("{id}={name}"))
        .collect::<Vec<_>>()
        .join(", ");
    let apps = ctx.known_apps.join(", ");

    format!(
        "You convert a voice-transcribed desktop command into exactly one JSON intent \
         matching the provided schema. Rules:\n\
         - Respond with JSON only.\n\
         - Numbers spoken as words (\"two\", \"five\") become integers.\n\
         - Outputs are referenced by index. Available outputs: {outputs}.\n\
         - Known application names: {apps}. Map synonyms to these when obvious \
           (\"browser\" -> \"firefox\").\n\
         - If the utterance is not clearly one of the intents, use the \"unknown\" \
           intent with the raw utterance. Never guess."
    )
}

#[derive(Deserialize)]
struct ChatResponse {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: String,
}

/// Voice text in, validated intent out. Any failure — HTTP, malformed JSON,
/// schema mismatch — surfaces as an `Err` and nothing reaches the IPC layer.
pub fn extract_intent(text: &str, ctx: &PromptContext) -> Result<VoiceIntent> {
    let schema = schemars::schema_for!(VoiceIntent);

    let body = serde_json::json!({
        "model": MODEL,
        "stream": false,
        "format": schema,
        "options": { "temperature": 0 },
        "messages": [
            { "role": "system", "content": system_prompt(ctx) },
            { "role": "user", "content": text },
        ],
    });

    let resp: ChatResponse = ureq::post(OLLAMA_URL)
        .timeout(std::time::Duration::from_secs(15))
        .send_json(body)
        .context("ollama request failed")?
        .into_json()
        .context("ollama response was not valid chat JSON")?;

    // The trust gate. This either yields a variant of VoiceIntent or an error.
    let intent: VoiceIntent = serde_json::from_str(resp.message.content.trim())
        .context("model output did not parse as VoiceIntent")?;

    Ok(intent)
}
