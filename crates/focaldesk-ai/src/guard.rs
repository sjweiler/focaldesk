//! Local, deterministic safety guards for AI requests and responses.
//!
//! These checks deliberately run before provider authorization, memory recall,
//! embedding, and logging. They are not a replacement for consent or provider
//! policy; they ensure sensitive values and relative-calendar boundaries are
//! handled before a model can reinterpret them.

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, Local, TimeZone};

use crate::{ChatMessage, ChatRequest, ChatRole, Citation};

const REDACTED: &str = "[REDACTED]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarWindow {
    pub start: DateTime<Local>,
    pub end: DateTime<Local>,
}

impl CalendarWindow {
    /// A half-open local-time interval covering the calendar month containing
    /// `now`. Calendar adapters must retain events satisfying
    /// `event.start < end && event.end > start`.
    pub fn this_month(now: DateTime<Local>) -> Self {
        let start = Local
            .with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0)
            .single()
            .expect("local midnight must be representable");
        let next = if now.month() == 12 {
            (now.year() + 1, 1)
        } else {
            (now.year(), now.month() + 1)
        };
        let end = Local
            .with_ymd_and_hms(next.0, next.1, 1, 0, 0, 0)
            .single()
            .expect("local midnight must be representable");
        Self { start, end }
    }

    pub fn overlaps(&self, event_start: DateTime<Local>, event_end: DateTime<Local>) -> bool {
        event_start < self.end && event_end > self.start
    }
}

/// Sanitizes locally supplied text before it can be logged, embedded, stored,
/// recalled, or transmitted to a provider. Direct requests to disclose secrets
/// or identity data are rejected rather than delegated to a model.
pub fn guard_request(mut request: ChatRequest, now: DateTime<Local>) -> Result<ChatRequest> {
    let latest = request
        .messages
        .iter()
        .rev()
        .find(|message| matches!(message.role, ChatRole::User))
        .map(|message| message.content.clone())
        .unwrap_or_default();
    if requests_sensitive_disclosure(&latest) {
        bail!("request blocked by the local privacy guard: it asks to disclose sensitive data");
    }

    if request
        .memory_sources
        .as_ref()
        .is_some_and(|sources| sources.len() > 1000 || sources.iter().any(|s| s.len() > 8192))
    {
        bail!("project retrieval scope exceeds source limits");
    }
    let mut image_bytes = 0;
    for message in &request.messages {
        for image in &message.images {
            let (header, data) = image
                .split_once(',')
                .ok_or_else(|| anyhow::anyhow!("image must be a data URL"))?;
            if !matches!(
                header,
                "data:image/png;base64" | "data:image/jpeg;base64" | "data:image/webp;base64"
            ) {
                bail!("unsupported image media type");
            }
            use base64::Engine as _;
            image_bytes += base64::engine::general_purpose::STANDARD
                .decode(data)?
                .len();
            if image_bytes > 8 * 1024 * 1024 {
                bail!("chat images exceed 8 MB");
            }
        }
    }
    for message in &mut request.messages {
        message.content = redact_sensitive_text(&message.content);
    }

    if asks_for_this_month(&latest) {
        let window = CalendarWindow::this_month(now);
        request.messages.push(ChatMessage::system(format!(
            "Calendar boundary is fixed by local code, not conversation history: use only events that overlap [{}, {}). Do not mention or infer events outside this interval. If no validated calendar records are supplied, say no events were found.",
            window.start.to_rfc3339(),
            window.end.to_rfc3339(),
        )));
    }
    Ok(request)
}

/// Returns the code-resolved window for a current-month calendar request.
/// Callers use this to route the request to the calendar store instead of
/// asking a language model to reconstruct events from chat history.
pub fn current_month_calendar_window(text: &str, now: DateTime<Local>) -> Option<CalendarWindow> {
    asks_for_this_month(text).then(|| CalendarWindow::this_month(now))
}

/// Produces a safe final response. Calendar claims require retrieved evidence;
/// secrets and PII are redacted; explicit sexual content is refused locally.
pub fn guard_output(prompt: &str, content: &str, citations: &[Citation]) -> String {
    if is_explicit_sexual_content(content) {
        return "I can’t provide explicit sexual content.".into();
    }
    if asks_for_this_month(prompt)
        && mentions_date_outside_window(content, &CalendarWindow::this_month(Local::now()))
    {
        return "I can’t use events outside the current calendar month. I’ll only report validated events from this month.".into();
    }
    if is_calendar_query(prompt) && citations.is_empty() && claims_calendar_events(content) {
        return "I can’t verify calendar events for that period because no validated calendar records were returned.".into();
    }
    redact_sensitive_text(content)
}

fn asks_for_this_month(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    is_calendar_query(&text) && (text.contains("this month") || text.contains("current month"))
}

fn is_calendar_query(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    ["calendar", "appointment", "meeting", "schedule", "events"]
        .iter()
        .any(|term| text.contains(term))
        || ((text.contains("this month") || text.contains("current month"))
            && [
                "what do i have",
                "what's coming up",
                "what is coming up",
                "coming up",
                "upcoming",
            ]
            .iter()
            .any(|term| text.contains(term)))
}

/// Reject dated output outside a code-resolved month.  A model may ignore a
/// system instruction or be steered by old chat history, so final output is
/// fail-closed when it names a clearly out-of-range date. Calendar adapters
/// remain responsible for retaining legitimate multi-day overlaps before the
/// event is rendered as text.
fn mentions_date_outside_window(text: &str, window: &CalendarWindow) -> bool {
    let lowercase = text.to_ascii_lowercase();
    let words = lowercase
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphabetic() && !c.is_ascii_digit()))
        .collect::<Vec<_>>();
    for pair in words.windows(2) {
        let Some(month) = month_number(pair[0]) else {
            continue;
        };
        let day = pair[1]
            .trim_end_matches(|c: char| c.is_ascii_alphabetic())
            .parse::<u32>()
            .ok();
        let Some(day) = day.filter(|day| (1..=31).contains(day)) else {
            continue;
        };
        let Some(date) = Local
            .with_ymd_and_hms(window.start.year(), month, day, 0, 0, 0)
            .single()
        else {
            continue;
        };
        if date < window.start || date >= window.end {
            return true;
        }
    }
    false
}

fn month_number(word: &str) -> Option<u32> {
    match word {
        "january" | "jan" => Some(1),
        "february" | "feb" => Some(2),
        "march" | "mar" => Some(3),
        "april" | "apr" => Some(4),
        "may" => Some(5),
        "june" | "jun" => Some(6),
        "july" | "jul" => Some(7),
        "august" | "aug" => Some(8),
        "september" | "sep" | "sept" => Some(9),
        "october" | "oct" => Some(10),
        "november" | "nov" => Some(11),
        "december" | "dec" => Some(12),
        _ => None,
    }
}

fn claims_calendar_events(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    [
        "you have",
        "your meeting",
        "your appointment",
        "scheduled",
        "on monday",
        "on tuesday",
        "on wednesday",
        "on thursday",
        "on friday",
    ]
    .iter()
    .any(|term| text.contains(term))
}

fn requests_sensitive_disclosure(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let disclose = ["reveal", "show", "list", "export", "tell me", "give me"]
        .iter()
        .any(|term| text.contains(term));
    let sensitive = [
        "password",
        "api key",
        "token",
        "secret",
        "social security",
        "ssn",
        "credit card",
        "private key",
    ]
    .iter()
    .any(|term| text.contains(term));
    disclose && sensitive
}

fn is_explicit_sexual_content(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let explicit = [
        "graphic sex",
        "explicit sexual",
        "pornographic",
        "sexual intercourse",
    ]
    .iter()
    .any(|term| text.contains(term));
    let medical = ["medical", "health", "anatomy", "consent", "sexual health"]
        .iter()
        .any(|term| text.contains(term));
    explicit && !medical
}

pub fn redact_sensitive_text(text: &str) -> String {
    let mut redacted = redact_secret_assignments(text);
    redacted = redact_email_addresses(&redacted);
    redact_phone_numbers(&redacted)
}

fn redact_secret_assignments(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            let secret = [
                "password",
                "passwd",
                "secret",
                "token",
                "api_key",
                "api-key",
                "authorization",
                "private_key",
                "private-key",
            ]
            .iter()
            .any(|key| lower.contains(key));
            if secret && (line.contains('=') || line.contains(':')) {
                let separator = if line.contains('=') { '=' } else { ':' };
                let key = line
                    .split_once(separator)
                    .map(|(key, _)| key)
                    .unwrap_or(line);
                format!(
                    "{key}{separator}{REDACTED}{}",
                    if line.ends_with("\r\n") {
                        "\r\n"
                    } else if line.ends_with('\n') {
                        "\n"
                    } else {
                        ""
                    }
                )
            } else {
                line.to_string()
            }
        })
        .collect()
}
fn map_tokens(text: &str, transform: impl Fn(&str) -> String) -> String {
    let mut output = String::new();
    let mut start = 0;
    for (index, character) in text.char_indices() {
        if character.is_whitespace() {
            if start < index {
                output.push_str(&transform(&text[start..index]));
            }
            output.push(character);
            start = index + character.len_utf8();
        }
    }
    if start < text.len() {
        output.push_str(&transform(&text[start..]));
    }
    output
}
fn redact_email_addresses(text: &str) -> String {
    map_tokens(text, |token| {
        let trimmed = token.trim_matches(|c: char| "<>()[]{}.,;:!?\"'".contains(c));
        if trimmed.contains('@')
            && trimmed.split_once('@').is_some_and(|(local, domain)| {
                !local.is_empty() && domain.contains('.') && !domain.starts_with('.')
            })
        {
            token.replacen(trimmed, REDACTED, 1)
        } else {
            token.to_string()
        }
    })
}
fn redact_phone_numbers(text: &str) -> String {
    map_tokens(text, |token| {
        let digits = token.chars().filter(char::is_ascii_digit).count();
        if (10..=16).contains(&digits)
            && token
                .chars()
                .all(|c| c.is_ascii_digit() || "+-(). ".contains(c))
        {
            REDACTED.to_string()
        } else {
            token.to_string()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{LocalResult, TimeZone};

    #[test]
    fn output_redaction_preserves_markdown_and_complete_secret_lines() {
        let output = redact_sensitive_text(
            "## Heading\n\n```rust\nlet count = 3;\n```\npassword: synthetic-value\nContact test@example.com\n",
        );
        assert!(output.contains("## Heading\n\n```rust\nlet count = 3;\n```"));
        assert!(!output.contains("synthetic-value"));
        assert!(!output.contains("test@example.com"));
        assert!(output.contains("password:[REDACTED]\n"));
    }

    fn local_time(year: i32, month: u32, day: u32, hour: u32) -> DateTime<Local> {
        match Local.with_ymd_and_hms(year, month, day, hour, 0, 0) {
            LocalResult::Single(time) => time,
            LocalResult::Ambiguous(time, _) => time,
            _ => panic!("test date must be unambiguous"),
        }
    }

    #[test]
    fn month_window_excludes_prior_month_and_keeps_overlap() {
        let window = CalendarWindow::this_month(local_time(2026, 11, 1, 12));
        assert!(!window.overlaps(local_time(2026, 10, 30, 9), local_time(2026, 10, 30, 10)));
        assert!(window.overlaps(local_time(2026, 10, 31, 23), local_time(2026, 11, 1, 1)));
        assert!(!window.overlaps(local_time(2026, 12, 1, 0), local_time(2026, 12, 1, 1)));
    }

    #[test]
    fn request_redacts_before_provider_and_adds_calendar_boundary() {
        let request = ChatRequest::from_prompt(
            "What is on my calendar this month? email me@example.com password=hunter2",
        );
        let guarded = guard_request(request, local_time(2026, 11, 1, 12)).unwrap();
        assert!(!guarded.messages[0].content.contains("me@example.com"));
        assert!(!guarded.messages[0].content.contains("hunter2"));
        assert!(
            guarded
                .messages
                .iter()
                .any(|message| message.content.contains("2026-11-01"))
        );
    }

    #[test]
    fn output_rejects_unsupported_calendar_claims_and_explicit_content() {
        assert!(
            guard_output("calendar this month", "You have a meeting on Monday.", &[])
                .contains("can’t verify")
        );
        assert_eq!(
            guard_output("hello", "This is explicit sexual content", &[]),
            "I can’t provide explicit sexual content."
        );
    }

    #[test]
    fn this_month_query_cannot_return_an_october_event_on_november_first() {
        let prompt = "what do i have coming up this month";
        assert!(asks_for_this_month(prompt));
        let window = CalendarWindow::this_month(local_time(2026, 11, 1, 12));
        assert!(mentions_date_outside_window(
            "MS AI Engineering — October 30th, 4:00 PM",
            &window
        ));
    }
}
