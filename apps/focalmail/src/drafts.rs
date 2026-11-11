use crate::{attachments::Attachment, Message};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Clone, Copy)]
pub enum Kind {
    Reply,
    ReplyAll,
    Forward,
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ForwardSource {
    pub account: String,
    pub message_id: String,
    pub attachments: Vec<Attachment>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Draft {
    pub id: String,
    pub from: String,
    pub to: String,
    pub cc: String,
    pub subject: String,
    pub body: String,
    #[serde(default)]
    pub html_body: Option<String>,
    #[serde(default)]
    pub signature_initialized: bool,
    pub thread_id: String,
    pub in_reply_to: String,
    pub references: String,
    pub reply_subject: String,
    pub reply_account: String,
    pub forward: Option<ForwardSource>,
}
impl Draft {
    pub fn new(from: String) -> Self {
        Self {
            id: format!("draft-{:032x}", rand::random::<u128>()),
            from,
            ..Default::default()
        }
    }
}
#[derive(Clone, Serialize)]
pub struct Summary {
    pub id: String,
    pub subject: String,
    pub to: String,
    pub from: String,
}

fn identity(address: &str) -> String {
    let address = address.to_lowercase();
    if let Some((local, domain)) = address.split_once('@') {
        if matches!(domain, "gmail.com" | "googlemail.com") {
            return format!(
                "{}@gmail.com",
                local.split('+').next().unwrap_or(local).replace('.', "")
            );
        }
    }
    address
}
pub fn addresses(value: &str) -> Vec<String> {
    let Ok(list) = mailparse::addrparse(value) else {
        return vec![];
    };
    list.iter()
        .flat_map(|item| match item {
            mailparse::MailAddr::Single(address) => vec![address.addr.clone()],
            mailparse::MailAddr::Group(group) => {
                group.addrs.iter().map(|a| a.addr.clone()).collect()
            }
        })
        .filter(|s| s.contains('@') && !s.chars().any(char::is_whitespace))
        .collect()
}
fn prefixed(subject: &str, prefix: &str) -> String {
    if subject
        .to_lowercase()
        .starts_with(&format!("{}:", prefix.to_lowercase()))
    {
        subject.into()
    } else {
        format!("{prefix}: {subject}")
    }
}
pub fn prepare(message: &Message, kind: Kind, accounts: &[String]) -> Draft {
    let mut draft = Draft::new(message.account.clone());
    if matches!(kind, Kind::Forward) {
        draft.subject = prefixed(&message.subject, "Fwd");
        draft.body = format!("\n\n---------- Forwarded message ----------\nFrom: {} <{}>\nDate: {}\nSubject: {}\nTo: {}\nCc: {}\n\n{}", message.sender, message.email, message.date, message.subject, message.to_header, message.cc_header, message.body);
        draft.forward = Some(ForwardSource {
            account: message.account.clone(),
            message_id: message.id.clone(),
            attachments: message.attachments.clone(),
        });
        return draft;
    }
    let mut own = accounts
        .iter()
        .map(|s| identity(s))
        .collect::<BTreeSet<_>>();
    own.insert(identity(&message.account));
    for address in addresses(&message.delivered_to) {
        own.insert(identity(&address));
    }
    let mut seen = own;
    let mut add = |items: Vec<String>| {
        items
            .into_iter()
            .filter(|s| seen.insert(identity(s)))
            .collect::<Vec<_>>()
    };
    let reply_to = addresses(&message.reply_to);
    let primary = if reply_to.is_empty() {
        vec![message.email.clone()]
    } else {
        reply_to
    };
    let mut to = add(primary);
    if to.is_empty() {
        to = add(addresses(&message.to_header));
    }
    let mut cc = Vec::new();
    if matches!(kind, Kind::ReplyAll) {
        cc.extend(add(addresses(&message.to_header)));
        cc.extend(add(addresses(&message.cc_header)));
    }
    if to.is_empty() && !cc.is_empty() {
        to.push(cc.remove(0));
    }
    draft.to = to.join(", ");
    draft.cc = cc.join(", ");
    draft.subject = prefixed(&message.subject, "Re");
    draft.body = format!(
        "\n\nOn {}, {} wrote:\n{}",
        message.date,
        message.sender,
        message
            .body
            .lines()
            .map(|line| format!("> {line}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    draft.reply_subject = draft.subject.clone();
    draft.reply_account = message.account.clone();
    draft.in_reply_to = message.message_id.clone();
    draft.references = format!("{} {}", message.references, message.message_id)
        .trim()
        .into();
    if !draft.in_reply_to.is_empty() {
        draft.thread_id = message.thread_id.clone();
    }
    draft
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message() -> Message {
        let mut message = crate::fixtures::messages().remove(0);
        message.account = "my.name@gmail.com".into();
        message.email = "sender@example.com".into();
        message.reply_to = "Support <reply@example.com>".into();
        message.to_header =
            "Me <myname+list@googlemail.com>, List: one@example.com, TWO@example.com;".into();
        message.cc_header =
            "one@example.com, two@example.com, other@work.test, three@example.com".into();
        message.message_id = "<original@example.com>".into();
        message.references = "<ancestor@example.com>".into();
        message.thread_id = "thread".into();
        message
    }
    #[test]
    fn reply_all_honors_reply_to_and_excludes_self_aliases_and_duplicates() {
        let draft = prepare(&message(), Kind::ReplyAll, &["other@work.test".into()]);
        assert_eq!(draft.to, "reply@example.com");
        assert_eq!(
            draft.cc,
            "one@example.com, TWO@example.com, three@example.com"
        );
        assert_eq!(draft.thread_id, "thread");
        assert_eq!(draft.in_reply_to, "<original@example.com>");
        assert!(draft
            .references
            .contains("<ancestor@example.com> <original@example.com>"));
        let reply = prepare(&message(), Kind::Reply, &[]);
        assert!(reply.cc.is_empty());
        assert_eq!(reply.to, "reply@example.com");
    }
    #[test]
    fn replying_to_own_sent_mail_targets_original_recipients() {
        let mut message = message();
        message.reply_to.clear();
        message.email = message.account.clone();
        let draft = prepare(&message, Kind::ReplyAll, &["other@work.test".into()]);
        assert_eq!(draft.to, "one@example.com, TWO@example.com");
        assert_eq!(draft.cc, "three@example.com");
    }
    #[test]
    fn forward_quotes_original_and_keeps_attachments_optional() {
        let mut message = message();
        message.subject = "Fwd: Existing".into();
        message.attachments.push(Attachment {
            name: "report.txt".into(),
            id: "part".into(),
            ..Default::default()
        });
        let draft = prepare(&message, Kind::Forward, &[]);
        assert_eq!(draft.subject, "Fwd: Existing");
        assert!(draft.to.is_empty() && draft.cc.is_empty() && draft.thread_id.is_empty());
        assert!(draft.body.contains("Forwarded message") && draft.body.contains(&message.body));
        assert_eq!(draft.forward.unwrap().attachments[0].name, "report.txt");
    }
}
