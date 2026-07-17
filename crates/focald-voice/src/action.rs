//! Intent -> DesktopAction mapping. Intents are *what the user meant*; actions are
//! *what the compositor executes*. The gap between them (app name -> exec line,
//! output index -> validated OutputId) is resolved here, in trusted code.
//!
use std::collections::HashMap;

use focaldesk_ipc::{DesktopAction, DesktopDirection};

use crate::intent::{Direction, VoiceIntent};

#[derive(Debug, Clone, Copy)]
pub struct OutputId(pub u32);

/// Compositor-side knowledge the mapper needs: real output topology and an
/// allowlist of launchable apps. In focald-voice proper, populate this from a
/// compositor IPC query at startup (and on hotplug events) plus a config file.
pub struct CompositorState {
    pub output_count: u32,
    /// alias -> exec line, e.g. "browser" -> "firefox", "terminal" -> "kitty"
    pub apps: HashMap<String, String>,
}

impl CompositorState {
    fn resolve_app(&self, name: &str) -> Option<String> {
        self.apps.get(&name.to_lowercase()).cloned()
    }

    fn valid_output(&self, idx: u32) -> Option<OutputId> {
        (idx < self.output_count).then_some(OutputId(idx))
    }
}

/// Reasons an intent didn't become an action — feed these back to the user
/// ("I don't know an app called X") rather than silently dropping.
#[derive(Debug)]
pub enum MapError {
    UnknownApp(String),
    BadOutput(u32),
    BadValue(String),
    Unrecognized(String),
}

pub fn to_action(intent: VoiceIntent, state: &CompositorState) -> Result<DesktopAction, MapError> {
    match intent {
        VoiceIntent::OpenApp { app, output } => {
            let exec = state
                .resolve_app(&app)
                .ok_or_else(|| MapError::UnknownApp(app))?;
            let output = match output {
                Some(idx) => Some(state.valid_output(idx).ok_or(MapError::BadOutput(idx))?),
                None => None,
            };
            // Validate the requested output even though launch placement is currently
            // selected by the compositor's normal launch policy.
            if let Some(output) = output {
                let _ = output.0;
            }
            Ok(DesktopAction::LaunchApp { app: exec })
        }

        VoiceIntent::FocusWorkspace { workspace } => {
            if workspace == 0 || workspace > 32 {
                return Err(MapError::BadValue(format!("workspace {workspace}")));
            }
            Ok(DesktopAction::FocusWorkspace { workspace })
        }

        VoiceIntent::MoveWindowToOutput { output } => state
            .valid_output(output)
            .map(|output| DesktopAction::MoveFocusedToOutput { output: output.0 })
            .ok_or(MapError::BadOutput(output)),

        VoiceIntent::MoveWindow { direction } => Ok(DesktopAction::MoveFocused {
            direction: match direction {
                Direction::Left => DesktopDirection::Left,
                Direction::Right => DesktopDirection::Right,
                Direction::Up => DesktopDirection::Up,
                Direction::Down => DesktopDirection::Down,
            },
        }),

        VoiceIntent::CloseWindow => Ok(DesktopAction::CloseFocused),

        VoiceIntent::SetVolume { percent } => {
            if percent > 100 {
                return Err(MapError::BadValue(format!("{percent}%")));
            }
            Ok(DesktopAction::SetVolume { percent })
        }

        VoiceIntent::Unknown { raw } => Err(MapError::Unrecognized(raw)),
    }
}
