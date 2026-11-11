//! Small, inert HTML vocabulary shared by outgoing mail and signatures.
use scraper::{ElementRef, Html, Node};
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Signature {
    pub html: String,
    pub new_messages: bool,
    pub replies_forwards: bool,
}

pub fn sanitize(source: &str) -> String {
    fn element(el: ElementRef<'_>, out: &mut String, depth: usize) {
        if depth > 64 {
            return;
        }
        let tag = el.value().name();
        if matches!(
            tag,
            "script"
                | "style"
                | "iframe"
                | "object"
                | "embed"
                | "svg"
                | "math"
                | "template"
                | "head"
                | "img"
                | "video"
                | "audio"
        ) {
            return;
        }
        let allowed = matches!(
            tag,
            "p" | "div"
                | "br"
                | "strong"
                | "b"
                | "em"
                | "i"
                | "u"
                | "ul"
                | "ol"
                | "li"
                | "a"
                | "blockquote"
        );
        if allowed {
            out.push('<');
            out.push_str(tag);
            if tag == "a" {
                if let Some(link) = el.attr("href").and_then(crate::html::safe_link) {
                    out.push_str(" href=\"");
                    out.push_str(&crate::html_layout::escape(&link));
                    out.push('"');
                }
            }
            // The marker tracks only the automatically inserted, editable signature.
            if tag == "div" && el.attr("data-focal-signature") == Some("true") {
                out.push_str(" data-focal-signature=\"true\"");
            }
            out.push('>');
        }
        for child in el.children() {
            match child.value() {
                Node::Text(text) => out.push_str(&crate::html_layout::escape(text)),
                Node::Element(_) => element(ElementRef::wrap(child).unwrap(), out, depth + 1),
                _ => {}
            }
        }
        if allowed && tag != "br" {
            out.push_str("</");
            out.push_str(tag);
            out.push('>');
        }
    }
    let document = Html::parse_fragment(source);
    let mut out = String::new();
    element(document.root_element(), &mut out, 0);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn outgoing_html_is_inert_and_preserves_basic_formatting() {
        let result = sanitize("<p style='background:url(https://tracker)' onclick='bad()'>Hi <b>there</b><img src='https://tracker'><script>bad()</script><a href='javascript:bad()'>bad</a><a href='https://example.com'>good</a></p>");
        assert_eq!(
            result,
            "<p>Hi <b>there</b><a>bad</a><a href=\"https://example.com/\">good</a></p>"
        );
    }
    #[test]
    fn sanitizer_is_stable_and_retains_signature_marker() {
        let html = "<div data-focal-signature=\"true\"><b>A &amp; B</b><br>Team</div>";
        assert_eq!(sanitize(html), html);
        assert_eq!(sanitize(&sanitize(html)), html);
    }
    #[test]
    fn signatures_and_html_drafts_persist_and_old_drafts_still_load() {
        let mut store =
            crate::store::Store::init(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let account = "me@example.com";
        let mut message = crate::fixtures::messages().remove(0);
        message.account = account.into();
        store.snapshot(account, &[message], "1").unwrap();
        assert!(store.signature(account).unwrap().html.is_empty());
        store
            .set_signature(
                account,
                &Signature {
                    html: "<b>Me</b><img src='https://tracker'>".into(),
                    new_messages: true,
                    replies_forwards: false,
                },
            )
            .unwrap();
        let signature = store.signature(account).unwrap();
        assert_eq!(signature.html, "<b>Me</b>");
        assert!(signature.new_messages && !signature.replies_forwards);
        assert!(store
            .signature("other@example.com")
            .unwrap()
            .html
            .is_empty());
        let mut draft = crate::drafts::Draft::new(account.into());
        draft.html_body = Some("<b>Saved</b><div data-focal-signature='true'>Me</div>".into());
        draft.signature_initialized = true;
        store.save_local_draft(&draft, Some(&[])).unwrap();
        let (loaded, _) = store.load_local_draft(&draft.id).unwrap();
        assert_eq!(loaded.html_body, draft.html_body.as_deref().map(sanitize));
        assert!(loaded.signature_initialized);
        let mut legacy = serde_json::to_value(&draft).unwrap();
        legacy.as_object_mut().unwrap().remove("html_body");
        legacy
            .as_object_mut()
            .unwrap()
            .remove("signature_initialized");
        let legacy: crate::drafts::Draft = serde_json::from_value(legacy).unwrap();
        assert!(legacy.html_body.is_none() && !legacy.signature_initialized);
        store.remove_account(account, || Ok(())).unwrap();
        assert!(store.signature(account).unwrap().html.is_empty());
        assert!(store.load_local_draft(&draft.id).is_ok());
    }
}
