use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provider {
    #[default]
    Gmail,
    Microsoft365,
    Manual,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Incoming {
    #[default]
    Imap,
    Pop3,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Authentication {
    #[default]
    OAuth,
    Password,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Security {
    #[default]
    Tls,
    StartTls,
}
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MicrosoftSignIn {
    #[default]
    Browser,
    DeviceCode,
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Profile {
    pub email: String,
    pub provider: Provider,
    pub incoming: Incoming,
    pub authentication: Authentication,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: Security,
    #[serde(default = "enabled")]
    pub smtp_copy_to_sent: bool,
    pub username: String,
    pub client_id: String,
    pub tenant: String,
    #[serde(default)]
    pub microsoft_sign_in: MicrosoftSignIn,
}
fn enabled() -> bool {
    true
}
impl Default for Profile {
    fn default() -> Self {
        Self {
            email: String::new(),
            provider: Provider::Gmail,
            incoming: Incoming::Imap,
            authentication: Authentication::OAuth,
            host: String::new(),
            port: 993,
            security: Security::Tls,
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_security: Security::StartTls,
            smtp_copy_to_sent: true,
            username: String::new(),
            client_id: String::new(),
            tenant: "common".into(),
            microsoft_sign_in: MicrosoftSignIn::Browser,
        }
    }
}
impl Profile {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.email.contains('@') && !self.email.chars().any(char::is_whitespace),
            "Enter an email address"
        );
        if self.provider == Provider::Microsoft365 {
            anyhow::ensure!(
                !self.client_id.trim().is_empty(),
                "Enter a Microsoft application client ID"
            );
            anyhow::ensure!(
                !self.tenant.is_empty()
                    && self
                        .tenant
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.'),
                "Invalid Microsoft tenant"
            );
        }
        if self.provider == Provider::Manual {
            anyhow::ensure!(
                !self.host.trim().is_empty() && !self.smtp_host.trim().is_empty(),
                "Enter incoming and outgoing server names"
            );
            anyhow::ensure!(
                self.port > 0 && self.smtp_port > 0,
                "Server ports must be greater than zero"
            );
        }
        Ok(())
    }
}

/// Rolling history retained by background synchronization. Server mail is untouched.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncHistory {
    OneMonth,
    ThreeMonths,
    SixMonths,
    #[default]
    OneYear,
    All,
}
impl SyncHistory {
    pub fn months(self) -> u32 {
        match self {
            Self::OneMonth => 1,
            Self::ThreeMonths => 3,
            Self::SixMonths => 6,
            Self::OneYear => 12,
            Self::All => 0,
        }
    }
    pub fn since(
        self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        (self != Self::All).then(|| {
            now.checked_sub_months(chrono::Months::new(self.months()))
                .expect("valid sync history")
        })
    }
    pub fn cutoff(self) -> Option<i64> {
        self.since(chrono::Utc::now()).map(|d| d.timestamp_millis())
    }
}

#[cfg(test)]
mod sync_history_tests {
    use super::*;
    #[test]
    fn history_uses_calendar_months_and_defaults_to_one_year() {
        let now = chrono::DateTime::parse_from_rfc3339("2024-03-31T12:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        assert_eq!(SyncHistory::default(), SyncHistory::OneYear);
        assert_eq!(
            SyncHistory::OneMonth.since(now).unwrap().to_rfc3339(),
            "2024-02-29T12:00:00+00:00"
        );
        assert_eq!(
            SyncHistory::OneYear.since(now).unwrap().to_rfc3339(),
            "2023-03-31T12:00:00+00:00"
        );
        assert!(SyncHistory::All.since(now).is_none());
        assert!(serde_json::from_str::<SyncHistory>("\"invalid\"").is_err());
    }
}
