//! Provider-neutral, restart-safe calendar storage.
//!
//! OAuth refresh tokens deliberately stay outside this crate. Callers obtain
//! them from `focald-secrets`, exchange them for short-lived access tokens,
//! and pass only the access token to the provider clients below.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, SecondsFormat, Utc};
use reqwest::blocking::Client;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;
use url::Url;

const MAX_PAGES: usize = 100;
const MAX_EVENTS_PER_SYNC: usize = 10_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CalendarProvider {
    Local,
    Google,
    Microsoft,
}

impl CalendarProvider {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Google => "google",
            Self::Microsoft => "microsoft",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CalendarEvent {
    pub account_id: String,
    pub provider: CalendarProvider,
    pub calendar_id: String,
    pub event_id: String,
    pub title: String,
    pub start_time: String,
    pub end_time: String,
    #[serde(default)]
    pub organizer: String,
    #[serde(default)]
    pub all_day: bool,
    #[serde(default)]
    pub cancelled: bool,
}

impl CalendarEvent {
    pub fn start_unix(&self) -> Result<i64> {
        parse_time(&self.start_time)
    }

    pub fn end_unix(&self) -> Result<i64> {
        parse_time(&self.end_time)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DueReminder {
    pub event: CalendarEvent,
    pub reminder_at_unix: i64,
}

#[derive(Debug, Clone, Default)]
pub struct SyncBatch {
    pub events: Vec<CalendarEvent>,
    pub deleted_event_ids: Vec<String>,
    pub next_cursor: Option<String>,
}

pub struct CalendarStore {
    connection: Mutex<Connection>,
}

impl CalendarStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create calendar state directory {}", parent.display()))?;
        }
        let connection = Connection::open(path)
            .with_context(|| format!("open calendar database {}", path.display()))?;
        connection.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS events (
               account_id TEXT NOT NULL,
               provider TEXT NOT NULL,
               calendar_id TEXT NOT NULL,
               event_id TEXT NOT NULL,
               title TEXT NOT NULL,
               start_time TEXT NOT NULL,
               end_time TEXT NOT NULL,
               start_unix INTEGER NOT NULL,
               end_unix INTEGER NOT NULL,
               organizer TEXT NOT NULL DEFAULT '',
               all_day INTEGER NOT NULL DEFAULT 0,
               cancelled INTEGER NOT NULL DEFAULT 0,
               updated_at INTEGER NOT NULL,
               PRIMARY KEY(account_id, calendar_id, event_id)
             );
             CREATE INDEX IF NOT EXISTS events_upcoming ON events(cancelled, start_unix);
             CREATE TABLE IF NOT EXISTS sync_cursors (
               account_id TEXT NOT NULL,
               calendar_id TEXT NOT NULL,
               cursor TEXT NOT NULL,
               updated_at INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(account_id, calendar_id)
             );
             CREATE TABLE IF NOT EXISTS delivered_reminders (
               account_id TEXT NOT NULL,
               calendar_id TEXT NOT NULL,
               event_id TEXT NOT NULL,
               start_unix INTEGER NOT NULL,
               delivered_at INTEGER NOT NULL,
               PRIMARY KEY(account_id, calendar_id, event_id, start_unix)
             );",
        )?;
        // Migration for databases created before cursor-age tracking existed.
        let _ = connection.execute(
            "ALTER TABLE sync_cursors ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
            [],
        );
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn apply_sync(&self, account_id: &str, calendar_id: &str, batch: &SyncBatch) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        let tx = connection.transaction()?;
        for event in &batch.events {
            let start_unix = event.start_unix()?;
            let end_unix = event.end_unix()?;
            tx.execute(
                "INSERT INTO events(account_id,provider,calendar_id,event_id,title,start_time,end_time,start_unix,end_unix,organizer,all_day,cancelled,updated_at)
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,unixepoch())
                 ON CONFLICT(account_id,calendar_id,event_id) DO UPDATE SET
                   provider=excluded.provider,title=excluded.title,start_time=excluded.start_time,
                   end_time=excluded.end_time,start_unix=excluded.start_unix,end_unix=excluded.end_unix,
                   organizer=excluded.organizer,all_day=excluded.all_day,cancelled=excluded.cancelled,
                   updated_at=excluded.updated_at",
                params![event.account_id,event.provider.as_str(),event.calendar_id,event.event_id,event.title,
                    event.start_time,event.end_time,start_unix,end_unix,event.organizer,event.all_day,event.cancelled],
            )?;
        }
        for event_id in &batch.deleted_event_ids {
            tx.execute(
                "DELETE FROM events WHERE account_id=?1 AND calendar_id=?2 AND event_id=?3",
                params![account_id, calendar_id, event_id],
            )?;
        }
        if let Some(cursor) = &batch.next_cursor {
            tx.execute(
                "INSERT INTO sync_cursors(account_id,calendar_id,cursor,updated_at) VALUES(?1,?2,?3,unixepoch())
                 ON CONFLICT(account_id,calendar_id) DO UPDATE SET cursor=excluded.cursor,updated_at=excluded.updated_at",
                params![account_id, calendar_id, cursor],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn cursor(&self, account_id: &str, calendar_id: &str) -> Result<Option<String>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        Ok(connection
            .query_row(
                "SELECT cursor FROM sync_cursors WHERE account_id=?1 AND calendar_id=?2",
                params![account_id, calendar_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn fresh_cursor(
        &self,
        account_id: &str,
        calendar_id: &str,
        now_unix: i64,
        max_age_seconds: i64,
    ) -> Result<Option<String>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        Ok(connection
            .query_row(
                "SELECT cursor FROM sync_cursors WHERE account_id=?1 AND calendar_id=?2 AND updated_at>=?3",
                params![account_id, calendar_id, now_unix - max_age_seconds],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub fn clear_calendar(&self, account_id: &str, calendar_id: &str) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        let tx = connection.transaction()?;
        tx.execute(
            "DELETE FROM events WHERE account_id=?1 AND calendar_id=?2",
            params![account_id, calendar_id],
        )?;
        tx.execute(
            "DELETE FROM sync_cursors WHERE account_id=?1 AND calendar_id=?2",
            params![account_id, calendar_id],
        )?;
        tx.execute(
            "DELETE FROM delivered_reminders WHERE account_id=?1 AND calendar_id=?2",
            params![account_id, calendar_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn clear_account(&self, account_id: &str) -> Result<()> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        let tx = connection.transaction()?;
        tx.execute("DELETE FROM events WHERE account_id=?1", [account_id])?;
        tx.execute("DELETE FROM sync_cursors WHERE account_id=?1", [account_id])?;
        tx.execute(
            "DELETE FROM delivered_reminders WHERE account_id=?1",
            [account_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn account_event_count(&self, account_id: &str) -> Result<u64> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        Ok(connection.query_row(
            "SELECT COUNT(*) FROM events WHERE account_id=?1",
            [account_id],
            |row| row.get(0),
        )?)
    }

    pub fn upcoming(
        &self,
        after_unix: i64,
        before_unix: i64,
        limit: usize,
    ) -> Result<Vec<CalendarEvent>> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        let mut statement = connection.prepare(
            "SELECT account_id,provider,calendar_id,event_id,title,start_time,end_time,organizer,all_day,cancelled
             FROM events WHERE cancelled=0 AND start_unix>=?1 AND start_unix<?2
             ORDER BY start_unix ASC LIMIT ?3")?;
        let rows = statement.query_map(
            params![after_unix, before_unix, limit.min(500) as i64],
            event_from_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn claim_due_reminders(
        &self,
        now_unix: i64,
        grace_seconds: i64,
        reminder_minutes_by_account: &BTreeMap<String, u32>,
    ) -> Result<Vec<DueReminder>> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| anyhow!("calendar store lock poisoned"))?;
        let tx = connection.transaction()?;
        let due: Vec<DueReminder> = {
            let mut statement = tx.prepare(
                "SELECT account_id,provider,calendar_id,event_id,title,start_time,end_time,organizer,all_day,cancelled,start_unix
                 FROM events e WHERE cancelled=0 AND start_unix>?1
                   AND start_unix<=?2
                   AND NOT EXISTS (SELECT 1 FROM delivered_reminders d WHERE d.account_id=e.account_id
                     AND d.calendar_id=e.calendar_id AND d.event_id=e.event_id AND d.start_unix=e.start_unix)
                 ORDER BY start_unix LIMIT 100")?;
            let rows = statement.query_map(
                params![now_unix - grace_seconds, now_unix + 60 * 60],
                |row| {
                    let event = event_from_row(row)?;
                    let start: i64 = row.get(10)?;
                    let minutes = reminder_minutes_by_account
                        .get(&event.account_id)
                        .copied()
                        .unwrap_or(10);
                    Ok(DueReminder {
                        event,
                        reminder_at_unix: start - i64::from(minutes) * 60,
                    })
                },
            )?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter(|reminder| {
                    reminder.reminder_at_unix <= now_unix
                        && reminder.reminder_at_unix > now_unix - grace_seconds
                        && reminder_minutes_by_account
                            .get(&reminder.event.account_id)
                            .copied()
                            .unwrap_or(10)
                            > 0
                })
                .collect()
        };
        for reminder in &due {
            tx.execute(
                "INSERT OR IGNORE INTO delivered_reminders(account_id,calendar_id,event_id,start_unix,delivered_at)
                 VALUES(?1,?2,?3,?4,?5)",
                params![reminder.event.account_id,reminder.event.calendar_id,reminder.event.event_id,
                    reminder.event.start_unix()?,now_unix],
            )?;
        }
        tx.execute(
            "DELETE FROM delivered_reminders WHERE delivered_at<?1",
            params![now_unix - 90 * 86_400],
        )?;
        tx.commit()?;
        Ok(due)
    }
}

fn event_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<CalendarEvent> {
    let provider: String = row.get(1)?;
    Ok(CalendarEvent {
        account_id: row.get(0)?,
        provider: match provider.as_str() {
            "google" => CalendarProvider::Google,
            "microsoft" => CalendarProvider::Microsoft,
            _ => CalendarProvider::Local,
        },
        calendar_id: row.get(2)?,
        event_id: row.get(3)?,
        title: row.get(4)?,
        start_time: row.get(5)?,
        end_time: row.get(6)?,
        organizer: row.get(7)?,
        all_day: row.get(8)?,
        cancelled: row.get(9)?,
    })
}

fn parse_time(value: &str) -> Result<i64> {
    if let Ok(value) = DateTime::parse_from_rfc3339(value) {
        return Ok(value.timestamp());
    }
    if let Ok(value) = chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return Ok(value
            .and_hms_opt(0, 0, 0)
            .context("invalid all-day time")?
            .and_utc()
            .timestamp());
    }
    bail!("unsupported calendar timestamp: {value}")
}

fn normalized_time(value: &Value) -> Result<(String, bool)> {
    if let Some(time) = value.get("dateTime").and_then(Value::as_str) {
        let parsed = DateTime::parse_from_rfc3339(time).context("invalid provider dateTime")?;
        return Ok((
            parsed.to_utc().to_rfc3339_opts(SecondsFormat::Secs, true),
            false,
        ));
    }
    if let Some(date) = value.get("date").and_then(Value::as_str) {
        parse_time(date)?;
        return Ok((format!("{date}T00:00:00Z"), true));
    }
    bail!("calendar event has no supported start/end time")
}

pub fn google_batch(account_id: &str, calendar_id: &str, response: &Value) -> Result<SyncBatch> {
    let mut batch = SyncBatch {
        next_cursor: response
            .get("nextSyncToken")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Default::default()
    };
    for item in response
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .context("Google event missing id")?;
        if id.len() > 1024 {
            bail!("Google event id is too large");
        }
        if item.get("status").and_then(Value::as_str) == Some("cancelled") {
            batch.deleted_event_ids.push(id.to_owned());
            continue;
        }
        let (start_time, all_day) =
            normalized_time(item.get("start").context("Google event missing start")?)?;
        let (end_time, _) = normalized_time(item.get("end").context("Google event missing end")?)?;
        batch.events.push(CalendarEvent {
            account_id: account_id.into(),
            provider: CalendarProvider::Google,
            calendar_id: calendar_id.into(),
            event_id: id.into(),
            title: bounded_text(
                item.get("summary")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled event)"),
                500,
            ),
            start_time,
            end_time,
            organizer: bounded_text(
                item.pointer("/organizer/email")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                320,
            ),
            all_day,
            cancelled: false,
        });
    }
    Ok(batch)
}

pub fn microsoft_batch(account_id: &str, calendar_id: &str, response: &Value) -> Result<SyncBatch> {
    let mut batch = SyncBatch {
        next_cursor: response
            .get("@odata.deltaLink")
            .and_then(Value::as_str)
            .map(str::to_owned),
        ..Default::default()
    };
    for item in response
        .get("value")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let id = item
            .get("id")
            .and_then(Value::as_str)
            .context("Microsoft event missing id")?;
        if id.len() > 1024 {
            bail!("Microsoft event id is too large");
        }
        if item.get("@removed").is_some()
            || item.get("isCancelled").and_then(Value::as_bool) == Some(true)
        {
            batch.deleted_event_ids.push(id.to_owned());
            continue;
        }
        let start = item
            .pointer("/start/dateTime")
            .and_then(Value::as_str)
            .context("Microsoft event missing start")?;
        let end = item
            .pointer("/end/dateTime")
            .and_then(Value::as_str)
            .context("Microsoft event missing end")?;
        // Ask Graph for UTC; a missing suffix is therefore safe to label as UTC.
        let start_time = if start.ends_with('Z') || start.contains('+') {
            start.into()
        } else {
            format!("{start}Z")
        };
        let end_time = if end.ends_with('Z') || end.contains('+') {
            end.into()
        } else {
            format!("{end}Z")
        };
        parse_time(&start_time)?;
        parse_time(&end_time)?;
        batch.events.push(CalendarEvent {
            account_id: account_id.into(),
            provider: CalendarProvider::Microsoft,
            calendar_id: calendar_id.into(),
            event_id: id.into(),
            title: bounded_text(
                item.get("subject")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled event)"),
                500,
            ),
            start_time,
            end_time,
            organizer: bounded_text(
                item.pointer("/organizer/emailAddress/address")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
                320,
            ),
            all_day: item
                .get("isAllDay")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            cancelled: false,
        });
    }
    Ok(batch)
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

pub struct CalendarHttpClient {
    client: Client,
}

impl CalendarHttpClient {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(30))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
        })
    }

    pub fn sync_google(
        &self,
        account_id: &str,
        calendar_id: &str,
        access_token: &str,
        cursor: Option<&str>,
    ) -> Result<SyncBatch> {
        let encoded: String =
            url::form_urlencoded::byte_serialize(calendar_id.as_bytes()).collect();
        let base = format!("https://www.googleapis.com/calendar/v3/calendars/{encoded}/events");
        let mut page_token: Option<String> = None;
        let mut combined = SyncBatch::default();
        for _ in 0..MAX_PAGES {
            let mut request = self.client.get(&base).bearer_auth(access_token).query(&[
                ("singleEvents", "true"),
                ("showDeleted", "true"),
                ("maxResults", "2500"),
            ]);
            if let Some(cursor) = cursor {
                request = request.query(&[("syncToken", cursor)]);
            } else {
                let now = Utc::now();
                let time_min =
                    (now - chrono::Duration::days(30)).to_rfc3339_opts(SecondsFormat::Secs, true);
                let time_max =
                    (now + chrono::Duration::days(365)).to_rfc3339_opts(SecondsFormat::Secs, true);
                request = request.query(&[("timeMin", &time_min), ("timeMax", &time_max)]);
            }
            if let Some(page) = page_token.as_deref() {
                request = request.query(&[("pageToken", page)]);
            }
            let response = request.send()?;
            if response.status() == reqwest::StatusCode::GONE {
                bail!("google_sync_cursor_expired");
            }
            let response = response.error_for_status()?.json::<Value>()?;
            let batch = google_batch(account_id, calendar_id, &response)?;
            combined.events.extend(batch.events);
            combined.deleted_event_ids.extend(batch.deleted_event_ids);
            if combined.events.len() + combined.deleted_event_ids.len() > MAX_EVENTS_PER_SYNC {
                bail!("Google calendar sync exceeds {MAX_EVENTS_PER_SYNC} changes");
            }
            if let Some(token) = response.get("nextPageToken").and_then(Value::as_str) {
                page_token = Some(token.into());
            } else {
                combined.next_cursor = batch.next_cursor;
                return Ok(combined);
            }
        }
        bail!("Google calendar sync exceeded {MAX_PAGES} pages")
    }

    pub fn sync_microsoft(
        &self,
        account_id: &str,
        calendar_id: &str,
        access_token: &str,
        cursor: Option<&str>,
        start: &str,
        end: &str,
    ) -> Result<SyncBatch> {
        let mut next = cursor.map(str::to_owned).unwrap_or_else(|| {
            let path = if calendar_id == "primary" {
                "me/calendarView/delta".to_string()
            } else {
                let encoded: String =
                    url::form_urlencoded::byte_serialize(calendar_id.as_bytes()).collect();
                format!("me/calendars/{encoded}/calendarView/delta")
            };
            format!(
                "https://graph.microsoft.com/v1.0/{path}?startDateTime={start}&endDateTime={end}"
            )
        });
        let mut combined = SyncBatch::default();
        for _ in 0..MAX_PAGES {
            let url = Url::parse(&next)?;
            if url.scheme() != "https" || url.host_str() != Some("graph.microsoft.com") {
                bail!("Microsoft delta link escaped graph.microsoft.com");
            }
            let response = self
                .client
                .get(url)
                .bearer_auth(access_token)
                .header("Prefer", "outlook.timezone=\"UTC\", odata.maxpagesize=500")
                .send()?
                .error_for_status()?
                .json::<Value>()?;
            let batch = microsoft_batch(account_id, calendar_id, &response)?;
            combined.events.extend(batch.events);
            combined.deleted_event_ids.extend(batch.deleted_event_ids);
            if combined.events.len() + combined.deleted_event_ids.len() > MAX_EVENTS_PER_SYNC {
                bail!("Microsoft calendar sync exceeds {MAX_EVENTS_PER_SYNC} changes");
            }
            if let Some(link) = response.get("@odata.nextLink").and_then(Value::as_str) {
                next = link.into();
            } else {
                combined.next_cursor = batch.next_cursor;
                return Ok(combined);
            }
        }
        bail!("Microsoft calendar sync exceeded {MAX_PAGES} pages")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_queries_and_claims_once() {
        let dir = tempfile::tempdir().unwrap();
        let store = CalendarStore::open(dir.path().join("calendar.db")).unwrap();
        let start = Utc::now().timestamp() + 300;
        let event = CalendarEvent {
            account_id: "a".into(),
            provider: CalendarProvider::Google,
            calendar_id: "primary".into(),
            event_id: "e".into(),
            title: "Interview".into(),
            start_time: DateTime::from_timestamp(start, 0).unwrap().to_rfc3339(),
            end_time: DateTime::from_timestamp(start + 3600, 0)
                .unwrap()
                .to_rfc3339(),
            organizer: "hiring@example.test".into(),
            all_day: false,
            cancelled: false,
        };
        store
            .apply_sync(
                "a",
                "primary",
                &SyncBatch {
                    events: vec![event],
                    next_cursor: Some("next".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.upcoming(start - 1, start + 1, 10).unwrap().len(), 1);
        assert_eq!(
            store
                .claim_due_reminders(
                    Utc::now().timestamp(),
                    900,
                    &BTreeMap::from([("a".into(), 10)]),
                )
                .unwrap()
                .len(),
            1
        );
        assert!(
            store
                .claim_due_reminders(
                    Utc::now().timestamp(),
                    900,
                    &BTreeMap::from([("a".into(), 10)]),
                )
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.cursor("a", "primary").unwrap().as_deref(),
            Some("next")
        );
    }

    #[test]
    fn parses_provider_deletions_without_retaining_bodies() {
        let google = serde_json::json!({"items":[{"id":"gone","status":"cancelled"},{"id":"ok","summary":"Class","start":{"dateTime":"2026-10-14T13:00:00Z"},"end":{"dateTime":"2026-10-14T14:00:00Z"},"description":"private"}],"nextSyncToken":"s"});
        let batch = google_batch("a", "primary", &google).unwrap();
        assert_eq!(batch.deleted_event_ids, ["gone"]);
        assert_eq!(batch.events[0].title, "Class");
        assert!(
            !serde_json::to_string(&batch.events)
                .unwrap()
                .contains("private")
        );
    }
}
