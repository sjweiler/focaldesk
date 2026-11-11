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
    let mut html = if html.is_empty() {
        None
    } else {
        Some(crate::html::parse(&html.join("\n")))
    };
    if let Some(doc) = &mut html {
        inline_images(&mail, doc, 0)?;
        doc.inline_attempted = true;
    }
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
fn inline_images(
    mail: &mailparse::ParsedMail<'_>,
    doc: &mut crate::html::Document,
    depth: usize,
) -> Result<()> {
    if depth > 32 {
        return Ok(());
    }
    if crate::html_layout::raster_type(&mail.ctype.mimetype) {
        if let Some(cid) = mail.headers.get_first_value("Content-ID") {
            let bytes = mail.get_body_raw()?;
            crate::html_layout::attach_inline(doc, &cid, &mail.ctype.mimetype, &bytes);
        }
    }
    for child in &mail.subparts {
        inline_images(child, doc, depth + 1)?;
    }
    Ok(())
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
    fn cid_images_are_attached_to_the_sanitized_layout_without_a_remote_request() {
        let raw=b"MIME-Version: 1.0\r\nContent-Type: multipart/related; boundary=related\r\n\r\n--related\r\nContent-Type: text/html\r\n\r\n<table><tr><td><img src=\"cid:logo\" alt=\"Logo\"></td></tr></table>\r\n--related\r\nContent-Type: image/gif\r\nContent-ID: <logo>\r\nContent-Transfer-Encoding: base64\r\n\r\nR0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7\r\n--related--\r\n";
        let message = parse(raw, "me@example.com", "cid-test", "Inbox").unwrap();
        let doc = message.html.unwrap();
        assert!(doc.layout.as_ref().unwrap().contains("<table"));
        assert_eq!(doc.images.len(), 1);
        assert!(doc.images[0]
            .inline_data
            .as_ref()
            .unwrap()
            .starts_with("data:image/gif;base64,"));
        assert!(!doc.needs_reload());
    }

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
    #[test]
    fn html_delivery_has_plain_alternative_and_nested_attachments() {
        let draft = crate::drafts::Draft {
            from: "me@example.com".into(), to: "you@example.com".into(),
            body: "Hello ✓\nTeam".into(),
            html_body: Some("<p>Hello <b>✓</b></p><div>Team</div><img src='https://tracker'><script>bad()</script>".into()),
            ..Default::default()
        };
        for files in [
            vec![],
            vec![crate::attachments::DraftAttachment {
                name: "résumé.bin".into(),
                data: vec![0, 1, 255],
            }],
        ] {
            let raw = crate::providers::mime(&draft, &files).unwrap();
            let parsed = mailparse::parse_mail(&raw).unwrap();
            let alternative = if files.is_empty() {
                &parsed
            } else {
                assert_eq!(parsed.ctype.mimetype, "multipart/mixed");
                assert_eq!(parsed.subparts[1].get_body_raw().unwrap(), vec![0, 1, 255]);
                &parsed.subparts[0]
            };
            assert_eq!(alternative.ctype.mimetype, "multipart/alternative");
            assert_eq!(alternative.subparts.len(), 2);
            assert_eq!(alternative.subparts[0].ctype.mimetype, "text/plain");
            assert_eq!(
                alternative.subparts[0].get_body().unwrap(),
                "Hello ✓\r\nTeam"
            );
            assert_eq!(alternative.subparts[1].ctype.mimetype, "text/html");
            assert_eq!(
                alternative.subparts[1].get_body().unwrap(),
                "<p>Hello <b>✓</b></p><div>Team</div>"
            );
        }
    }
}
