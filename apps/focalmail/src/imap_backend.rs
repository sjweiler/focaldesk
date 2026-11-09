use crate::{
    drafts::Draft,
    providers::{cancelled, Action},
    settings::{Authentication, Incoming, Profile, Security},
    store::Store,
    Message,
};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use imap::types::{Flag, NameAttribute};
use std::{
    net::{TcpStream, ToSocketAddrs},
    sync::{atomic::AtomicBool, Arc},
    time::Duration,
};
type Session = imap::Session<native_tls::TlsStream<TcpStream>>;
fn entry(email: &str) -> Result<keyring::Entry> {
    Ok(keyring::Entry::new("org.focaldesk.FocalMail.imap", email)?)
}
fn validate(p: &Profile) -> Result<()> {
    p.validate()?;
    anyhow::ensure!(
        p.incoming == Incoming::Imap,
        "POP3 is not supported yet; choose IMAP"
    );
    anyhow::ensure!(
        p.authentication == Authentication::Password,
        "Use a password/app password; generic IMAP OAuth is not supported yet"
    );
    Ok(())
}
fn username(p: &Profile) -> &str {
    if p.username.trim().is_empty() {
        &p.email
    } else {
        &p.username
    }
}
fn session(p: &Profile, password: &str) -> Result<Session> {
    validate(p)?;
    let addresses = (p.host.as_str(), p.port).to_socket_addrs()?;
    let mut stream = None;
    let mut last_error = None;
    for address in addresses {
        match TcpStream::connect_timeout(&address, Duration::from_secs(10)) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let stream = stream
        .ok_or_else(|| {
            last_error.unwrap_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    "No mail server address",
                )
            })
        })
        .context("Cannot connect to incoming mail server")?;
    stream.set_read_timeout(Some(Duration::from_secs(20)))?;
    stream.set_write_timeout(Some(Duration::from_secs(20)))?;
    let tls = native_tls::TlsConnector::builder()
        .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
        .build()?;
    let client = if p.security == Security::Tls {
        let mut c = imap::Client::new(
            tls.connect(&p.host, stream)
                .context("Incoming TLS handshake failed")?,
        );
        c.read_greeting()?;
        c
    } else {
        let mut c = imap::Client::new(stream);
        c.read_greeting()?;
        c.secure(&p.host, &tls)?
    };
    client
        .login(username(p), password)
        .map_err(|(error, _)| login_error(error))
}
fn login_error(error: imap::error::Error) -> anyhow::Error {
    match error {
        imap::error::Error::No(_) => crate::service::ReconnectRequired.into(),
        error => error.into(),
    }
}
fn smtp(p: &Profile, password: &str) -> Result<lettre::SmtpTransport> {
    use lettre::transport::smtp::{
        authentication::Credentials,
        client::{Tls, TlsParameters},
    };
    let params = TlsParameters::new(p.smtp_host.clone())?;
    Ok(lettre::SmtpTransport::builder_dangerous(&p.smtp_host)
        .port(p.smtp_port)
        .tls(if p.smtp_security == Security::Tls {
            Tls::Wrapper(params)
        } else {
            Tls::Required(params)
        })
        .credentials(Credentials::new(username(p).into(), password.into()))
        .timeout(Some(Duration::from_secs(30)))
        .build())
}
pub fn connect(p: &Profile, password: &str, cancel: Arc<AtomicBool>) -> Result<String> {
    validate(p)?;
    crate::providers::ensure_provider(p)?;
    anyhow::ensure!(!password.is_empty(), "Enter a password or app password");
    cancelled(&cancel)?;
    let mut session = session(p, password)?;
    let _ = session.logout();
    cancelled(&cancel)?;
    anyhow::ensure!(
        smtp(p, password)?.test_connection()?,
        "SMTP connection test failed"
    );
    cancelled(&cancel)?;
    entry(&p.email)?.set_password(password)?;
    Store::open()?.connect_profile(p)?;
    let result = sync(p, &cancel);
    Store::open()?.record_status(&p.email, &result)?;
    result?;
    Ok(format!("Connected {}", p.email))
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Identity {
    mailbox: String,
    validity: u32,
    uid: u32,
}
fn encode(mailbox: &str, validity: u32, uid: u32) -> Result<String> {
    Ok(URL_SAFE_NO_PAD.encode(serde_json::to_vec(&Identity {
        mailbox: mailbox.into(),
        validity,
        uid,
    })?))
}
fn identity(id: &str) -> Result<Identity> {
    let i: Identity = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(id)?)?;
    anyhow::ensure!(i.uid > 0, "Invalid IMAP UID");
    Ok(i)
}
fn folder(name: &str, attrs: &[NameAttribute<'_>]) -> String {
    if name.eq_ignore_ascii_case("INBOX") {
        return "Inbox".into();
    }
    for (special, label) in [
        ("\\Sent", "Sent"),
        ("\\Trash", "Trash"),
        ("\\Junk", "Spam"),
        ("\\Drafts", "Server Drafts"),
        ("\\Archive", "Archive"),
    ] {
        if attrs
            .iter()
            .any(|a| matches!(a,NameAttribute::Custom(s) if s.eq_ignore_ascii_case(special)))
        {
            return label.into();
        }
    }
    // Common servers omit SPECIAL-USE attributes from LIST.
    match name.to_ascii_lowercase().as_str() {
        "sent" | "sent items" | "sent messages" => "Sent",
        "trash" | "deleted items" => "Trash",
        "spam" | "junk" => "Spam",
        "drafts" => "Server Drafts",
        _ => "Archive",
    }
    .into()
}
fn destination(name: &str, attrs: &[NameAttribute<'_>], target: &str) -> bool {
    if attrs.contains(&NameAttribute::NoSelect) {
        return false;
    }
    let special = match target {
        "Archive" => "\\Archive",
        "Trash" => "\\Trash",
        "Sent" => "\\Sent",
        _ => return false,
    };
    attrs
        .iter()
        .any(|a| matches!(a, NameAttribute::Custom(s) if s.eq_ignore_ascii_case(special)))
        || match target {
            "Archive" => matches!(name.to_ascii_lowercase().as_str(), "archive" | "archives"),
            "Trash" => matches!(
                name.to_ascii_lowercase().as_str(),
                "trash" | "deleted items"
            ),
            "Sent" => matches!(
                name.to_ascii_lowercase().as_str(),
                "sent" | "sent items" | "sent messages"
            ),
            _ => false,
        }
}
pub fn sync(p: &Profile, cancel: &AtomicBool) -> Result<()> {
    let password = entry(&p.email)?
        .get_password()
        .map_err(crate::service::credential_error)?;
    let mut session = session(p, &password)?;
    let cached = Store::open()?
        .load()?
        .into_iter()
        .filter(|m| m.account == p.email)
        .map(|m| (m.id.clone(), m))
        .collect::<std::collections::HashMap<_, _>>();
    let progress = Store::open()?;
    progress.sync_progress(&p.email, "scanning_folders", 0, None)?;
    let since = progress.sync_history(&p.email)?.since(chrono::Utc::now());
    let messages = scan(&mut session, p, cancel, &cached, since, |n| {
        progress.sync_progress(&p.email, "downloading", n, None)
    })?;
    progress.sync_progress(&p.email, "saving", messages.len(), Some(messages.len()))?;
    cancelled(cancel)?;
    Store::open()?.snapshot(&p.email, &messages, "imap-full-v1")?;
    progress.prune_sync_history(&p.email)?;
    let _ = session.logout();
    Ok(())
}
fn scan<T: std::io::Read + std::io::Write>(
    session: &mut imap::Session<T>,
    p: &Profile,
    cancel: &AtomicBool,
    cached: &std::collections::HashMap<String, Message>,
    since: Option<chrono::DateTime<chrono::Utc>>,
    mut progress: impl FnMut(usize) -> Result<()>,
) -> Result<Vec<Message>> {
    let names = session.list(Some(""), Some("*"))?;
    let folders = names
        .iter()
        .filter(|n| !n.attributes().contains(&NameAttribute::NoSelect))
        .map(|n| (n.name().to_string(), folder(n.name(), n.attributes())))
        .collect::<Vec<_>>();
    let mut messages = Vec::new();
    for (mailbox, label) in folders {
        cancelled(cancel)?;
        let selected = session.examine(&mailbox)?;
        let validity = selected
            .uid_validity
            .context("Server omitted UIDVALIDITY")?;
        if selected.exists == 0 {
            continue;
        }
        let query = since
            .map(|d| format!("SINCE {}", d.format("%d-%b-%Y")))
            .unwrap_or_else(|| "ALL".into());
        let mut uids = session.uid_search(query)?.into_iter().collect::<Vec<_>>();
        uids.sort_unstable_by(|a, b| b.cmp(a));
        // Bound command size on large mailboxes, and import newest mail first.
        for batch in uids.chunks(200) {
            cancelled(cancel)?;
            let set = batch
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(",");
            let fetched = session.uid_fetch(set, "(UID FLAGS RFC822.SIZE INTERNALDATE)")?;
            let metadata = fetched
                .iter()
                .map(|f| {
                    (
                        f.uid,
                        f.size,
                        f.flags().to_vec(),
                        f.internal_date().map(|d| d.timestamp_millis()),
                    )
                })
                .collect::<Vec<_>>();
            for (uid, _size, flags, received) in metadata {
                cancelled(cancel)?;
                if flags.contains(&Flag::Deleted) {
                    continue;
                }
                let uid = uid.context("Server omitted UID")?;
                let id = encode(&mailbox, validity, uid)?;
                let mut message = if let Some(m) = cached.get(&id) {
                    m.clone()
                } else {
                    // Initial sync fetches headers only; opening the message retrieves
                    // its MIME content and attachments through the existing lazy path.
                    let fetched = session.uid_fetch(uid.to_string(), "BODY.PEEK[HEADER]")?;
                    let raw = fetched
                        .iter()
                        .next()
                        .and_then(|f| f.header())
                        .context("Message vanished during sync; refresh again")?;
                    let mut m = crate::mime::parse(raw, &p.email, &id, &label)?;
                    m.attachments_loaded = false;
                    m.html_loaded = false;
                    m
                };
                if let Some(received) = received {
                    message.timestamp = received;
                }
                message.folder = label.clone();
                message.unread = !flags.contains(&Flag::Seen);
                message.flagged = flags.contains(&Flag::Flagged);
                messages.push(message);
                progress(messages.len())?;
            }
        }
    }
    Ok(messages)
}
fn select(session: &mut Session, id: &Identity) -> Result<()> {
    let mailbox = session.select(&id.mailbox)?;
    anyhow::ensure!(
        mailbox.uid_validity == Some(id.validity),
        "Mailbox identity changed; refresh before modifying mail"
    );
    Ok(())
}
pub fn load_content(p: &Profile, m: &Message) -> Result<Message> {
    if m.attachments_loaded && m.html_loaded && m.html.as_ref().is_none_or(|h| !h.needs_reload()) {
        return Ok(m.clone());
    }
    let password = entry(&p.email)?.get_password()?;
    let mut session = session(p, &password)?;
    let id = identity(&m.id)?;
    select(&mut session, &id)?;
    let metadata = session.uid_fetch(id.uid.to_string(), "RFC822.SIZE")?;
    let size = metadata
        .iter()
        .next()
        .and_then(|f| f.size)
        .context("Message unavailable")?;
    anyhow::ensure!(size <= 40 * 1024 * 1024, "Message exceeds 40 MiB");
    let fetched = session.uid_fetch(id.uid.to_string(), "BODY.PEEK[]")?;
    let bytes = fetched
        .iter()
        .next()
        .and_then(|f| f.body())
        .context("Message unavailable")?;
    let mut loaded = crate::mime::parse(bytes, &m.account, &m.id, &m.folder)?;
    loaded.timestamp = m.timestamp;
    loaded.unread = m.unread;
    loaded.flagged = m.flagged;
    let _ = session.logout();
    Ok(loaded)
}
pub struct MutationSession {
    session: Session,
}
impl MutationSession {
    pub fn open(p: &Profile) -> Result<Self> {
        let password = entry(&p.email)?.get_password()?;
        Ok(Self {
            session: session(p, &password)?,
        })
    }
    pub fn apply(&mut self, m: &Message, action: Action) -> Result<()> {
        let id = identity(&m.id)?;
        select(&mut self.session, &id)?;
        match action {
            Action::Read(read) => {
                self.session.uid_store(
                    id.uid.to_string(),
                    if read {
                        "+FLAGS.SILENT (\\Seen)"
                    } else {
                        "-FLAGS.SILENT (\\Seen)"
                    },
                )?;
            }
            Action::Star(star) => {
                self.session.uid_store(
                    id.uid.to_string(),
                    if star {
                        "+FLAGS.SILENT (\\Flagged)"
                    } else {
                        "-FLAGS.SILENT (\\Flagged)"
                    },
                )?;
            }
            Action::Archive | Action::Trash => {
                anyhow::ensure!(
                    self.session.capabilities()?.has_str("MOVE"),
                    "Server does not support safe UID MOVE"
                );
                let names = self.session.list(Some(""), Some("*"))?;
                let target = if matches!(action, Action::Trash) {
                    "Trash"
                } else {
                    "Archive"
                };
                let destination = names
                    .iter()
                    .find(|n| {
                        !n.attributes().contains(&NameAttribute::NoSelect)
                            && n.name() != id.mailbox
                            && destination(n.name(), n.attributes(), target)
                    })
                    .context("Destination folder not found on mail server")?;
                self.session
                    .uid_mv(id.uid.to_string(), destination.name())?;
            }
        }
        Ok(())
    }
}
impl Drop for MutationSession {
    fn drop(&mut self) {
        let _ = self.session.logout();
    }
}
pub fn forget_credentials(email: &str) -> Result<()> {
    match entry(email)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
pub fn modify(p: &Profile, m: &Message, action: Action, cancel: &AtomicBool) -> Result<String> {
    cancelled(cancel)?;
    let mut session = MutationSession::open(p)?;
    session.apply(m, action)?;
    drop(session);
    match sync(p, cancel) {
        Ok(()) => Ok("Mail updated".into()),
        Err(e) => Ok(format!("Mail updated; refresh needed: {e}")),
    }
}
pub fn send(p: &Profile, draft: &Draft, raw: &[u8], cancel: &AtomicBool) -> Result<String> {
    use lettre::{
        address::{Address, Envelope},
        Transport,
    };
    let password = entry(&p.email)?.get_password()?;
    validate(p)?;
    let recipients = crate::drafts::addresses(&draft.to)
        .into_iter()
        .chain(crate::drafts::addresses(&draft.cc))
        .map(|a| a.parse::<Address>())
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let envelope = Envelope::new(Some(p.email.parse()?), recipients)?;
    cancelled(cancel)?;
    smtp(p, &password)?
        .send_raw(&envelope, raw)
        .context("SMTP send failed or outcome unknown; check Sent before retrying")?;
    if !p.smtp_copy_to_sent {
        return Ok("Message sent · server manages Sent copies".into());
    }
    // Delivery succeeded; never report a Sent-copy failure as a failed send.
    let copy = (|| -> Result<()> {
        let mut session = session(p, &password)?;
        let names = session.list(Some(""), Some("*"))?;
        let sent = names
            .iter()
            .find(|n| destination(n.name(), n.attributes(), "Sent"))
            .context("Sent folder unavailable")?;
        session.append_with_flags(sent.name(), raw, &[Flag::Seen])?;
        let _ = session.logout();
        Ok(())
    })();
    Ok(if copy.is_ok() {
        "Message sent · copied to Sent".into()
    } else {
        "Message sent; could not confirm a copy in Sent. Do not resend.".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
    };
    #[test]
    fn uid_identity_includes_folder_and_uidvalidity_and_move_uses_real_destinations() {
        let first = encode("Folder/日本語", 1, 42).unwrap();
        assert_ne!(first, encode("Folder/日本語", 2, 42).unwrap());
        assert_ne!(first, encode("Inbox", 1, 42).unwrap());
        let decoded = identity(&first).unwrap();
        assert_eq!(decoded.mailbox, "Folder/日本語");
        assert_eq!(decoded.uid, 42);
        assert!(!destination("Clients", &[], "Archive"));
        assert!(destination("Archives", &[], "Archive"));
        assert!(destination(
            "Localized",
            &[NameAttribute::Custom("\\Trash".into())],
            "Trash"
        ));
        assert!(!destination("Trash", &[NameAttribute::NoSelect], "Trash"));
    }
    #[test]
    fn starttls_refusal_never_transmits_credentials() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(b"* OK test\r\n").unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.contains("STARTTLS"));
            assert!(!line.contains("secret-password"));
            let tag = line.split_whitespace().next().unwrap();
            writeln!(stream, "{tag} NO TLS unavailable\r").unwrap();
            let mut rest = String::new();
            let _ = reader.read_line(&mut rest);
            assert!(!rest.contains("LOGIN"));
            assert!(!rest.contains("secret-password"));
        });
        let p = Profile {
            email: "me@example.com".into(),
            provider: crate::settings::Provider::Manual,
            authentication: Authentication::Password,
            host: "127.0.0.1".into(),
            port,
            security: Security::StartTls,
            smtp_host: "smtp.example.com".into(),
            ..Default::default()
        };
        assert!(session(&p, "secret-password").is_err());
        worker.join().unwrap();
    }
    #[test]
    fn login_connection_loss_is_retryable_but_rejected_credentials_require_reconnect() {
        let disconnected = login_error(imap::error::Error::ConnectionLost);
        assert!(!disconnected.is::<crate::service::ReconnectRequired>());
        assert!(crate::service::is_network_error(&disconnected));
        let timeout = login_error(imap::error::Error::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "test",
        )));
        assert!(crate::service::is_network_error(&timeout));
        let rejected = login_error(imap::error::Error::No("AUTHENTICATIONFAILED".into()));
        assert!(rejected.is::<crate::service::ReconnectRequired>());
    }
    #[test]
    fn smtp_starttls_refusal_never_transmits_authentication() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(b"220 test ESMTP\r\n").unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            assert!(line.starts_with("EHLO"));
            stream
                .write_all(b"250-test\r\n250-STARTTLS\r\n250 AUTH PLAIN LOGIN\r\n")
                .unwrap();
            line.clear();
            reader.read_line(&mut line).unwrap();
            assert_eq!(line.trim(), "STARTTLS");
            stream.write_all(b"454 TLS unavailable\r\n").unwrap();
            line.clear();
            let _ = reader.read_line(&mut line);
            assert!(!line.contains("AUTH"));
        });
        let p = Profile {
            smtp_host: "127.0.0.1".into(),
            smtp_port: port,
            smtp_security: Security::StartTls,
            ..Default::default()
        };
        assert!(smtp(&p, "secret-password")
            .unwrap()
            .test_connection()
            .is_err());
        worker.join().unwrap();
    }
    #[test]
    fn imap_scan_reuses_cache_updates_flags_and_fetches_new_mail_without_marking_read() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let raw =
            "From: sender@example.com\r\nTo: me@example.com\r\nSubject: New mail\r\n\r\nHello"
                .to_string();
        let expected = raw.clone();
        let worker = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            stream.write_all(b"* OK test\r\n").unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            for operation in [
                "LOGIN",
                "LIST",
                "EXAMINE",
                "UID SEARCH",
                "UID FETCH",
                "UID FETCH",
            ] {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.contains(operation), "{line}");
                let tag = line.split_whitespace().next().unwrap();
                match operation {
                    "LIST"=>stream.write_all(b"* LIST () \"/\" \"INBOX\"\r\n").unwrap(),
                    "EXAMINE"=>stream.write_all(b"* 3 EXISTS\r\n* OK [UIDVALIDITY 7] valid\r\n").unwrap(),
                    "UID SEARCH"=>{assert!(line.contains("SINCE 08-Nov-2025"),"{line}");stream.write_all(b"* SEARCH 1 2 3\r\n").unwrap()},
                    "UID FETCH" if line.contains("RFC822.SIZE")=>stream.write_all(b"* 1 FETCH (UID 1 FLAGS (\\Seen \\Flagged) RFC822.SIZE 80)\r\n* 2 FETCH (UID 2 FLAGS () RFC822.SIZE 80)\r\n* 3 FETCH (UID 3 FLAGS (\\Deleted) RFC822.SIZE 80)\r\n").unwrap(),
                    "UID FETCH"=>{
                        assert!(line.contains("UID FETCH 2 BODY.PEEK[HEADER]"),"{line}");
                        write!(stream,"* 2 FETCH (UID 2 BODY[HEADER] {{{}}}\r\n{})\r\n",raw.len(),raw).unwrap();
                    },_=>{}
                }
                write!(stream, "{tag} OK completed\r\n").unwrap();
            }
        });
        let mut client = imap::Client::new(TcpStream::connect(address).unwrap());
        client.read_greeting().unwrap();
        let mut session = client.login("test", "test").unwrap();
        let p = Profile {
            email: "me@example.com".into(),
            ..Default::default()
        };
        let id = encode("INBOX", 7, 1).unwrap();
        let old =
            crate::mime::parse(b"Subject: Old\r\n\r\nCached", &p.email, &id, "Inbox").unwrap();
        let cached = std::collections::HashMap::from([(id, old)]);
        let messages = scan(
            &mut session,
            &p,
            &AtomicBool::new(false),
            &cached,
            Some(
                chrono::DateTime::parse_from_rfc3339("2025-11-08T00:00:00Z")
                    .unwrap()
                    .with_timezone(&chrono::Utc),
            ),
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(messages.len(), 2);
        assert!(!messages[0].unread);
        assert!(messages[0].flagged);
        assert_eq!(messages[0].body, "Cached");
        assert!(messages[1].unread);
        assert!(!messages[1].attachments_loaded);
        assert!(!messages[1].html_loaded);
        assert_eq!(messages[1].subject, "New mail");
        assert!(expected.contains(&messages[1].body));
        worker.join().unwrap();
    }
}
