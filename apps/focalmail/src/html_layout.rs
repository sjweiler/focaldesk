//! Serialize an allowlisted layout. URLs never enter image `src` attributes.
use crate::html::{safe_link, Document};
use scraper::{ElementRef, Node};
use serde::{Deserialize, Serialize};
pub const MAX_INLINE_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_IMAGE_TOTAL: usize = 10 * 1024 * 1024;
#[derive(Clone, Serialize, Deserialize)]
pub struct Image {
    pub index: usize,
    pub alt: String,
    pub source: Option<String>,
    pub inline_data: Option<String>,
}
pub fn raster_type(value: &str) -> bool {
    matches!(
        value,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}
pub fn data_image(mime: &str, bytes: &[u8]) -> Option<String> {
    use base64::Engine;
    let signature = match mime {
        "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
        "image/jpeg" => bytes.starts_with(b"\xff\xd8\xff"),
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/webp" => bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
        _ => false,
    };
    (signature && bytes.len() <= MAX_INLINE_BYTES).then(|| {
        format!(
            "data:{mime};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        )
    })
}
pub fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}
fn style(value: &str) -> String {
    value
        .split(';')
        .filter_map(|rule| {
            let (key, value) = rule.split_once(':')?;
            let key = key.trim().to_ascii_lowercase();
            if !matches!(
                key.as_str(),
                "color"
                    | "background-color"
                    | "font-family"
                    | "font-size"
                    | "font-weight"
                    | "font-style"
                    | "text-align"
                    | "text-decoration"
                    | "line-height"
                    | "letter-spacing"
                    | "vertical-align"
                    | "width"
                    | "max-width"
                    | "min-width"
                    | "height"
                    | "max-height"
                    | "padding"
                    | "padding-top"
                    | "padding-bottom"
                    | "padding-left"
                    | "padding-right"
                    | "margin"
                    | "margin-top"
                    | "margin-bottom"
                    | "margin-left"
                    | "margin-right"
                    | "border"
                    | "border-color"
                    | "border-width"
                    | "border-style"
                    | "border-collapse"
                    | "border-radius"
                    | "display"
            ) {
                return None;
            }
            let value = value.trim();
            if value.len() > 160
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b" #.,%+-/'\"()".contains(&b))
            {
                return None;
            }
            let lower = value.to_ascii_lowercase();
            if lower.contains('(')
                && !(["rgb(", "rgba(", "hsl(", "hsla("]
                    .iter()
                    .any(|p| lower.starts_with(p))
                    && lower.ends_with(')')
                    && lower[lower.find('(')? + 1..lower.len() - 1]
                        .bytes()
                        .all(|b| b.is_ascii_digit() || b" .,%+-/".contains(&b)))
            {
                return None;
            }
            Some(format!("{key}:{value}"))
        })
        .collect::<Vec<_>>()
        .join(";")
}
struct Layout {
    html: String,
    images: Vec<Image>,
    nodes: usize,
    limited: bool,
}
impl Layout {
    fn element(&mut self, e: ElementRef<'_>, depth: usize) {
        self.nodes += 1;
        if depth > 64 || self.nodes > 20_000 || self.html.len() > crate::html::MAX_HTML * 2 {
            self.limited = true;
            return;
        }
        let tag = e.value().name();
        if matches!(
            tag,
            "head"
                | "script"
                | "style"
                | "iframe"
                | "object"
                | "embed"
                | "svg"
                | "math"
                | "template"
                | "noscript"
                | "form"
                | "input"
                | "button"
                | "textarea"
                | "select"
                | "video"
                | "audio"
                | "canvas"
                | "link"
                | "meta"
                | "base"
        ) || e.attr("hidden").is_some()
            || e.attr("aria-hidden") == Some("true")
        {
            return;
        }
        let allowed = matches!(
            tag,
            "p" | "div"
                | "span"
                | "section"
                | "article"
                | "header"
                | "footer"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
                | "ul"
                | "ol"
                | "li"
                | "blockquote"
                | "pre"
                | "code"
                | "b"
                | "strong"
                | "i"
                | "em"
                | "u"
                | "s"
                | "del"
                | "small"
                | "sub"
                | "sup"
                | "br"
                | "hr"
                | "table"
                | "thead"
                | "tbody"
                | "tfoot"
                | "tr"
                | "td"
                | "th"
                | "colgroup"
                | "col"
                | "a"
                | "img"
                | "center"
                | "font"
        );
        if allowed {
            self.html.push('<');
            self.html.push_str(tag);
            let css = style(e.attr("style").unwrap_or_default());
            if !css.is_empty() {
                self.html.push_str(&format!(" style=\"{}\"", escape(&css)));
            }
            for attr in [
                "width",
                "height",
                "colspan",
                "rowspan",
                "cellpadding",
                "cellspacing",
                "border",
            ] {
                if let Some(value) = e
                    .attr(attr)
                    .filter(|v| v.len() < 8 && v.bytes().all(|b| b.is_ascii_digit() || b == b'%'))
                {
                    self.html
                        .push_str(&format!(" {attr}=\"{}\"", escape(value)));
                }
            }
            for attr in ["align", "valign", "bgcolor", "color"] {
                if let Some(value) = e.attr(attr).filter(|v| {
                    v.len() < 32 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'#')
                }) {
                    self.html
                        .push_str(&format!(" {attr}=\"{}\"", escape(value)));
                }
            }
            if tag == "a" {
                if let Some(url) = e.attr("href").and_then(safe_link) {
                    self.html.push_str(&format!(
                        " href=\"{}\" rel=\"noreferrer noopener\"",
                        escape(&url)
                    ));
                }
            }
            if tag == "img" {
                let index = self.images.len();
                let alt = e
                    .attr("alt")
                    .unwrap_or("Image")
                    .chars()
                    .take(200)
                    .collect::<String>();
                let source = e.attr("src").and_then(|src| {
                    if let Some(cid) = src.strip_prefix("cid:").filter(|c| c.len() < 512) {
                        Some(format!("cid:{cid}"))
                    } else {
                        safe_link(src).filter(|url| url.starts_with("https://"))
                    }
                });
                self.html.push_str(&format!(
                    " data-mail-image=\"{index}\" alt=\"{}\"",
                    escape(&alt)
                ));
                if index < 100 {
                    self.images.push(Image {
                        index,
                        alt,
                        source,
                        inline_data: None,
                    });
                } else {
                    self.limited = true;
                }
            }
            self.html.push('>');
        }
        if tag != "img" {
            for child in e.children() {
                if self.nodes >= 20_000 || self.html.len() > crate::html::MAX_HTML * 2 {
                    self.limited = true;
                    break;
                }
                match child.value() {
                    Node::Text(t) => {
                        self.nodes += 1;
                        self.html.push_str(&escape(t));
                    }
                    Node::Element(_) => {
                        if let Some(e) = ElementRef::wrap(child) {
                            self.element(e, depth + 1);
                        }
                    }
                    _ => {}
                }
            }
        }
        if allowed && !matches!(tag, "br" | "hr" | "img" | "col") {
            self.html.push_str("</");
            self.html.push_str(tag);
            self.html.push('>');
        }
    }
}
pub fn attach(source: &str, doc: &mut Document) {
    let parsed = scraper::Html::parse_document(source);
    let mut out = Layout {
        html: String::new(),
        images: vec![],
        nodes: 0,
        limited: false,
    };
    out.element(parsed.root_element(), 0);
    doc.layout = Some(out.html);
    doc.images = out.images;
    doc.limited |= out.limited;
}
pub fn attach_inline(doc: &mut Document, cid: &str, mime: &str, bytes: &[u8]) {
    let mut used = doc
        .images
        .iter()
        .filter_map(|i| i.inline_data.as_ref())
        .map(String::len)
        .sum::<usize>();
    let Some(data) = data_image(mime, bytes) else {
        return;
    };
    let cid = cid.trim().trim_start_matches('<').trim_end_matches('>');
    for image in &mut doc.images {
        if used + data.len() > MAX_IMAGE_TOTAL {
            break;
        }
        if image.source.as_deref() == Some(&format!("cid:{cid}")) {
            used += data.len();
            image.inline_data = Some(data.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newsletter_tables_and_safe_styles_survive_without_active_content() {
        let doc = crate::html::parse(
            r##"<style>@import 'https://tracker.test/css';</style><table width="600" cellpadding="12" style="background-color:#fff;color:rgb(10, 20, 30);width:100%;background-image:url(https://tracker.test/bg);position:fixed"><tr><td colspan="2"><h1>Travel &amp; ideas</h1><p style="font-family:'Open Sans',sans-serif;font-size:18px">Hello <strong>reader</strong></p><img src="https://example.com/hero.jpg" srcset="https://tracker.test/2x" onerror="evil()" alt="Hero"><a href="https://example.com/offers" ping="https://tracker.test/ping">Explore</a><a href="javascript:evil()">Bad</a><iframe srcdoc="evil"></iframe><form action="https://tracker.test"><input></form><svg onload="evil()"></svg></td></tr></table>"##,
        );
        let layout = doc.layout.unwrap();
        assert!(layout.contains("<table") && layout.contains("colspan=\"2\""));
        assert!(
            layout.contains("background-color:#fff") && layout.contains("color:rgb(10, 20, 30)")
        );
        assert!(layout.contains("font-size:18px") && layout.contains("Travel &amp; ideas"));
        assert!(layout.contains("data-mail-image=\"0\""));
        for forbidden in [
            "<script",
            "<style",
            "<iframe",
            "<form",
            "<svg",
            "onerror",
            "srcset",
            " src=",
            "ping=",
            "javascript:",
            "background-image",
            "position:",
            "@import",
        ] {
            assert!(!layout.contains(forbidden), "{forbidden}: {layout}");
        }
        assert_eq!(doc.images.len(), 1);
        assert_eq!(
            doc.images[0].source.as_deref(),
            Some("https://example.com/hero.jpg")
        );
    }
    #[test]
    fn css_url_obfuscation_and_active_image_formats_are_not_accepted() {
        for css in [
            "background-image:url(x)",
            "color:expression(alert(1))",
            "color:var(--secret)",
            "color:r\\67 b(1,2,3)",
            "font-family:url(https://tracker.test)",
            "width:attr(data-secret)",
        ] {
            assert_eq!(style(css), "");
        }
        assert!(data_image("image/svg+xml", b"<svg onload='evil()'>").is_none());
        assert!(data_image("image/png", b"<html>not an image</html>").is_none());
        assert!(data_image("image/png", &vec![0; MAX_INLINE_BYTES + 1]).is_none());
        let doc=crate::html::parse("<img src='data:image/svg+xml,evil'><img src='file:///etc/passwd'><img src='http://example.com/tracker'>");
        assert!(doc.images.iter().all(|i| i.source.is_none()));
    }
    #[test]
    fn cached_old_documents_still_deserialize_and_request_an_upgrade() {
        let old: Document =
            serde_json::from_str(r#"{"blocks":[],"blocked_images":1,"limited":false}"#).unwrap();
        assert!(old.needs_reload());
    }
}
