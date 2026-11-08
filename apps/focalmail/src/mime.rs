//! MIME decoding shared by IMAP and Microsoft Graph's raw-message endpoint.
use crate::{
    attachments::{Attachment, MAX_BYTES},
    Message,
};
use anyhow::Result;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use mailparse::MailHeaderMap;
pub fn parse(raw: &[u8], account: &str, id: &str, folder: &str) -> Result<Message> {
    anyhow::ensure!(raw.len() <= 40 * 1024 * 1024, "Message exceeds 40 MiB");
    let mail = mailparse::parse_mail(raw)?;
    let h = |key: &str| mail.headers.get_first_value(key).unwrap_or_default();
    let from = h("From");
    let address = mailparse::addrparse(&from)
        .ok()
        .and_then(|a| a.extract_single_info());
    let (sender, email) = address
        .map(|a| (a.display_name.unwrap_or_else(|| a.addr.clone()), a.addr))
        .unwrap_or((from.clone(), from));
    let mut plain = Vec::new();
    let mut html = Vec::new();
    let mut attachments = Vec::new();
    parts(&mail, &mut plain, &mut html, &mut attachments, 0)?;
    let html = if html.is_empty() {
        None
    } else {
        Some(crate::html::parse(&html.join("\n")))
    };
    let body = if plain.is_empty() {
        html.as_ref().map(|h| h.plain_text()).unwrap_or_default()
    } else {
        plain.join("\n\n")
    };
    Ok(Message {
        id: id.into(),
        account: account.into(),
        sender,
        email,
        subject: h("Subject"),
        date: h("Date"),
        timestamp: mailparse::dateparse(&h("Date")).unwrap_or_default() * 1000,
        body,
        folder: folder.into(),
        unread: false,
        flagged: false,
        labels: vec![],
        attachments,
        attachments_loaded: true,
        html,
        html_loaded: true,
        to_header: h("To"),
        cc_header: h("Cc"),
        reply_to: h("Reply-To"),
        delivered_to: h("Delivered-To"),
        message_id: h("Message-ID"),
        references: h("References"),
        thread_id: String::new(),
        recipients_loaded: true,
    })
}
fn parts(
    mail: &mailparse::ParsedMail<'_>,
    plain: &mut Vec<String>,
    html: &mut Vec<String>,
    attachments: &mut Vec<Attachment>,
    depth: usize,
) -> Result<()> {
    anyhow::ensure!(
        depth <= 32 && attachments.len() < 256,
        "Message MIME structure is too large"
    );
    let disposition = mail.get_content_disposition();
    let name = disposition
        .params
        .get("filename")
        .or_else(|| mail.ctype.params.get("name"));
    if disposition.disposition == mailparse::DispositionType::Attachment || name.is_some() {
        let bytes = mail.get_body_raw()?;
        attachments.push(Attachment {
            id: format!("mime-{}", attachments.len()),
            name: crate::attachments::filename(
                name.map(String::as_str).unwrap_or("attachment.bin"),
            ),
            size: bytes.len() as u64,
            data: if bytes.len() <= MAX_BYTES {
                URL_SAFE_NO_PAD.encode(bytes)
            } else {
                String::new()
            },
        });
    } else if mail.subparts.is_empty() {
        match mail.ctype.mimetype.as_str() {
            "text/plain" => plain.push(mail.get_body()?),
            "text/html" => html.push(mail.get_body()?),
            _ => {}
        }
    } else {
        for part in &mail.subparts {
            parts(part, plain, html, attachments, depth + 1)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multipart_decodes_reply_headers_safe_html_and_binary_attachments() {
        let raw = b"From: Alice <alice@example.com>\r\nTo: me@example.com\r\nCc: other@example.com\r\nReply-To: reply@example.com\r\nMessage-ID: <original@example.com>\r\nReferences: <older@example.com>\r\nSubject: =?UTF-8?B?SGVsbG8=?=\r\nDate: Thu, 05 Nov 2026 12:00:00 +0000\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=test\r\n\r\n--test\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>Hello <b>there</b></p><script>danger()</script><img src=\"https://tracker.invalid/pixel\">\r\n--test\r\nContent-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=report.bin\r\nContent-Transfer-Encoding: base64\r\n\r\nAAEC/w==\r\n--test--\r\n";
        let m = parse(raw, "me@example.com", "id", "Inbox").unwrap();
        assert_eq!(m.subject, "Hello");
        assert_eq!(m.email, "alice@example.com");
        assert!(m.body.contains("Hello"));
        assert!(!m.body.contains("danger()"));
        assert_eq!(m.reply_to, "reply@example.com");
        assert_eq!(m.message_id, "<original@example.com>");
        assert!(m.timestamp > 0);
        assert_eq!(m.attachments.len(), 1);
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&m.attachments[0].data).unwrap(),
            [0, 1, 2, 255]
        );
    }
    #[test]
    fn outgoing_mime_roundtrips_cc_unicode_and_attachments() {
        let draft = crate::drafts::Draft {
            from: "me@example.com".into(),
            to: "you@example.com".into(),
            cc: "other@example.com".into(),
            subject: "Hello ✓".into(),
            body: "Text ✓\nsecond line".into(),
            ..Default::default()
        };
        let files = vec![crate::attachments::DraftAttachment {
            name: "test.txt".into(),
            data: b"hello".to_vec(),
        }];
        let raw = crate::providers::mime(&draft, &files).unwrap();
        let m = parse(&raw, &draft.from, "id", "Sent").unwrap();
        assert_eq!(m.subject, draft.subject);
        assert_eq!(m.cc_header, draft.cc);
        assert!(m.body.contains("Text ✓"));
        assert!(m.message_id.contains("@focalmail.local>"));
        assert!(!m.date.is_empty());
        assert_eq!(
            URL_SAFE_NO_PAD.decode(&m.attachments[0].data).unwrap(),
            b"hello"
        );
        let bad = crate::drafts::Draft {
            subject: "bad\r\nBcc: victim@example.com".into(),
            ..draft
        };
        assert!(crate::providers::mime(&bad, &[]).is_err());
    }
}
