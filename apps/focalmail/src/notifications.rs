//! Content-free desktop notifications: no subject, sender or body on the lock screen.
use crate::store::Store;
use std::collections::HashSet;
pub fn candidates<'a>(
    messages: &'a [crate::Message],
    account: &str,
    before: &HashSet<String>,
) -> Vec<&'a crate::Message> {
    messages
        .iter()
        .filter(|m| {
            m.account == account && m.unread && m.in_folder("Inbox") && !before.contains(&m.id)
        })
        .collect()
}
pub fn new_mail(account: &str, before: &HashSet<String>) {
    let _ = (|| -> anyhow::Result<()> {
        let store = Store::open()?;
        if !store.notifications_enabled()? {
            return Ok(());
        }
        let messages = store.load()?;
        let count = candidates(&messages, account, before).len();
        if count == 0 {
            return Ok(());
        }
        let connection = zbus::blocking::connection::Builder::session()?
            .method_timeout(std::time::Duration::from_secs(3))
            .build()?;
        let proxy = zbus::blocking::Proxy::new(
            &connection,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )?;
        let hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
            std::collections::HashMap::new();
        let _: u32 = proxy.call(
            "Notify",
            &(
                "FocalMail",
                0u32,
                "mail-unread",
                "New mail",
                format!("{count} new unread message(s)"),
                Vec::<&str>::new(),
                hints,
                8000i32,
            ),
        )?;
        Ok(())
    })(); // A missing notification server must never fail mail sync.
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notify_only_new_unread_inbox_messages_for_the_synced_account() {
        let mut m = crate::mime::parse(
            b"Subject: Example\r\n\r\nBody",
            "me@example.com",
            "new",
            "Inbox",
        )
        .unwrap();
        m.unread = true;
        let mut old = m.clone();
        old.id = "old".into();
        let mut read = m.clone();
        read.id = "read".into();
        read.unread = false;
        let mut sent = m.clone();
        sent.id = "sent".into();
        sent.folder = "Sent".into();
        let mut other = m.clone();
        other.account = "other@example.com".into();
        let messages = vec![m, old, read, sent, other];
        let before = HashSet::from(["old".into()]);
        let found = candidates(&messages, "me@example.com", &before);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].id, "new");
    }
}
