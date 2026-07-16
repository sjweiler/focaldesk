//! Zero-latency path for exact phrasings. If this matches, the LLM is never
//! invoked — the utterance goes straight to the same `VoiceIntent` type the
//! LLM would have produced, so downstream code can't tell the difference.

use crate::intent::{Direction, VoiceIntent};

/// Try to match common command shapes without inference.
/// Returns None to fall through to the LLM.
pub fn try_match(text: &str) -> Option<VoiceIntent> {
    let t = text
        .trim()
        .trim_end_matches(['.', ',', '!', '?'])
        .to_lowercase();
    let words: Vec<&str> = t.split_whitespace().collect();

    match words.as_slice() {
        ["open", app] | ["launch", app] | ["start", app] => Some(VoiceIntent::OpenApp {
            app: (*app).to_string(),
            output: None,
        }),

        ["open", "the", app] | ["launch", "the", app] | ["start", "the", app] => {
            Some(VoiceIntent::OpenApp {
                app: (*app).to_string(),
                output: None,
            })
        }

        ["close", "window"] | ["close", "this", "window"] => Some(VoiceIntent::CloseWindow),

        ["workspace", n] | ["go", "to", "workspace", n] | ["switch", "to", "workspace", n] => {
            parse_num(n).map(|workspace| VoiceIntent::FocusWorkspace { workspace })
        }

        ["move", "window", dir] | ["move", "the", "window", dir] => {
            parse_dir(dir).map(|direction| VoiceIntent::MoveWindow { direction })
        }

        ["volume", n] | ["set", "volume", "to", n] | ["set", "volume", n] => parse_num(n)
            .filter(|&p| p <= 100)
            .map(|p| VoiceIntent::SetVolume { percent: p as u8 }),

        _ => None,
    }
}

fn parse_num(s: &str) -> Option<u32> {
    let s = s.trim_end_matches('%').trim_end_matches("percent");
    if let Ok(n) = s.parse() {
        return Some(n);
    }
    // STT engines often spell small numbers out
    Some(match s {
        "zero" => 0,
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        _ => return None,
    })
}

fn parse_dir(s: &str) -> Option<Direction> {
    Some(match s {
        "left" => Direction::Left,
        "right" => Direction::Right,
        "up" => Direction::Up,
        "down" => Direction::Down,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_app_launches_do_not_require_the_llm() {
        for (utterance, expected) in [
            ("open browser", "browser"),
            ("Open settings.", "settings"),
            ("launch the terminal", "terminal"),
        ] {
            let Some(VoiceIntent::OpenApp { app, output }) = try_match(utterance) else {
                panic!("{utterance:?} did not match an app launch");
            };
            assert_eq!(app, expected);
            assert_eq!(output, None);
        }
    }
}
