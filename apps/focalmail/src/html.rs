use scraper::{ElementRef, Html, Node};
use serde::{Deserialize, Serialize};

pub const MAX_HTML: usize = 1024 * 1024;
const MAX_NODES: usize = 20_000;
const MAX_DEPTH: usize = 64;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Document {
    pub blocks: Vec<Block>,
    pub blocked_images: usize,
    pub limited: bool,
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub images: Vec<crate::html_layout::Image>,
    #[serde(default)]
    pub inline_attempted: bool,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Block {
    pub spans: Vec<Span>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Span {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub heading: u8,
    pub link: Option<String>,
}

pub fn safe_link(value: &str) -> Option<String> {
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return None;
    }
    let url = url::Url::parse(value.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }
    Some(url.to_string())
}

struct Builder {
    document: Document,
    block: Block,
    nodes: usize,
    emitted_bytes: usize,
}
impl Builder {
    fn flush(&mut self) {
        if self.block.spans.iter().any(|s| !s.text.trim().is_empty()) {
            self.document.blocks.push(std::mem::take(&mut self.block));
        } else {
            self.block.spans.clear();
        }
    }
    fn text(&mut self, value: &str, style: &Span) {
        let text = if style.code {
            value.to_string()
        } else {
            let mut text = String::new();
            let mut space = self
                .block
                .spans
                .last()
                .and_then(|s| s.text.chars().last())
                .is_none_or(char::is_whitespace);
            for c in value.chars() {
                if c.is_whitespace() {
                    if !space {
                        text.push(' ');
                    }
                    space = true;
                } else {
                    text.push(c);
                    space = false;
                }
            }
            text
        };
        self.emitted_bytes += text.len() + style.link.as_ref().map_or(0, String::len);
        if self.emitted_bytes > MAX_HTML * 2 {
            self.document.limited = true;
            return;
        }
        if !text.is_empty() {
            self.block.spans.push(Span {
                text,
                ..style.clone()
            });
        }
    }
    fn element(&mut self, element: ElementRef<'_>, mut style: Span, depth: usize) {
        self.nodes += 1;
        if self.nodes > MAX_NODES || depth > MAX_DEPTH {
            self.document.limited = true;
            return;
        }
        let tag = element.value().name();
        // Only native text/style primitives survive; nothing is passed to a browser.
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
        ) || element.attr("hidden").is_some()
            || element.attr("aria-hidden") == Some("true")
        {
            return;
        }
        let block = matches!(
            tag,
            "p" | "div"
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
                | "table"
                | "tr"
                | "hr"
        );
        if block {
            self.flush();
        }
        match tag {
            "b" | "strong" | "th" => style.bold = true,
            "i" | "em" => style.italic = true,
            "code" | "pre" => style.code = true,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                style.heading = tag.as_bytes()[1] - b'0';
                style.bold = true;
            }
            "a" => style.link = element.attr("href").and_then(safe_link),
            "li" => self.text("• ", &style),
            "blockquote" => self.text("❯ ", &style),
            "br" => {
                self.flush();
                return;
            }
            "hr" => {
                self.text("────────", &style);
                self.flush();
                return;
            }
            "img" | "picture" => {
                self.document.blocked_images += 1;
                let alt = element.attr("alt").unwrap_or("Image");
                self.text(
                    &format!(
                        "[Image blocked: {}]",
                        alt.chars().take(200).collect::<String>()
                    ),
                    &style,
                );
                return;
            }
            _ => {}
        }
        for child in element.children() {
            if self.nodes >= MAX_NODES {
                self.document.limited = true;
                break;
            }
            match child.value() {
                Node::Text(text) => {
                    self.nodes += 1;
                    self.text(text, &style);
                }
                Node::Element(_) => {
                    if let Some(child) = ElementRef::wrap(child) {
                        self.element(child, style.clone(), depth + 1);
                    }
                }
                _ => {}
            }
        }
        if matches!(tag, "td" | "th") {
            self.text(" | ", &Span::default());
        }
        if block {
            self.flush();
        }
    }
}

pub fn parse(source: &str) -> Document {
    if source.len() > MAX_HTML {
        return Document {
            limited: true,
            ..Default::default()
        };
    }
    let html = Html::parse_document(source);
    let mut builder = Builder {
        document: Document::default(),
        block: Block::default(),
        nodes: 0,
        emitted_bytes: 0,
    };
    builder.element(html.root_element(), Span::default(), 0);
    builder.flush();
    crate::html_layout::attach(source, &mut builder.document);
    builder.document
}

impl Document {
    pub fn needs_reload(&self) -> bool {
        self.layout.is_none()
            || (!self.inline_attempted
                && self.images.iter().any(|i| {
                    i.source.as_ref().is_some_and(|s| s.starts_with("cid:"))
                        && i.inline_data.is_none()
                }))
    }
    pub fn plain_text(&self) -> String {
        self.blocks
            .iter()
            .map(|b| b.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readable_formatting_entities_lists_and_tables_survive() {
        let doc = parse("<h1>Title &amp; news</h1><p>Hello <strong>world</strong> <em>today</em>.</p><ul><li>One</li><li>Two</li></ul><table><tr><td>A</td><td>B</td></tr></table>");
        assert!(doc.plain_text().contains("Title & news"));
        assert!(doc
            .blocks
            .iter()
            .flat_map(|b| &b.spans)
            .any(|s| s.bold && s.text == "world"));
        assert!(doc
            .blocks
            .iter()
            .flat_map(|b| &b.spans)
            .any(|s| s.italic && s.text == "today"));
        assert!(doc.plain_text().contains("• One"));
        assert!(doc.plain_text().contains("A | B"));
    }
    #[test]
    fn active_content_tracking_and_unsafe_links_never_reach_renderer() {
        let doc = parse(
            r#"<base href="https://evil.test"><meta http-equiv="refresh" content="0;url=https://evil.test"><script>secret_script</script><style>secret_style</style><iframe srcdoc="secret_frame">secret_frame</iframe><form>secret_form</form><svg><text>secret_svg</text></svg><p onclick="secret_handler">Visible<img src="https://evil.test/pixel" srcset="https://evil.test/2x" alt="Logo"></p><a href="javascript:secret_code">bad</a><a href="https://example.com/page">good</a>"#,
        );
        assert!(!doc.plain_text().contains("secret_"));
        assert!(doc.plain_text().contains("Visible"));
        assert_eq!(doc.blocked_images, 1);
        let links = doc
            .blocks
            .iter()
            .flat_map(|b| &b.spans)
            .filter_map(|s| s.link.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(links, ["https://example.com/page"]);
        for value in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "//example.com",
            "https://user:pass@example.com",
            "mailto:a@example.com",
            "https://example.com/\n",
        ] {
            assert!(safe_link(value).is_none(), "{value}");
        }
    }
    #[test]
    fn malformed_and_large_documents_are_bounded() {
        assert!(parse("<p>Unclosed <b>bold &lt;safe&gt;")
            .plain_text()
            .contains("bold <safe>"));
        assert!(parse(&"a".repeat(MAX_HTML + 1)).limited);
        assert!(
            parse(&format!(
                "{}text{}",
                "<div>".repeat(100),
                "</div>".repeat(100)
            ))
            .limited
        );
    }
}
