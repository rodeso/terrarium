//! Markdown → HTML for assistant replies.
//!
//! Runs on both the server (first render) and in the browser (while a reply
//! streams in). Model output is untrusted, so raw HTML is shown as text and
//! links are restricted to safe URL schemes.

use crate::emoji;
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd, html};

pub fn render(src: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    // Emoji are wrapped in spans except where markup isn't allowed or wanted.
    let mut in_code_block = false;
    let mut in_image = false;

    let events = Parser::new_ext(src, options).map(move |event| match event {
        // Never pass model-written HTML through: show it as text.
        Event::Html(t) | Event::InlineHtml(t) | Event::Text(t) => {
            if in_code_block || in_image || !emoji::may_contain_emoji(&t) {
                Event::Text(t)
            } else {
                Event::Html(CowStr::from(emoji::to_html(&t)))
            }
        }

        // Wrap code blocks with a header showing the language and a copy button.
        Event::Start(Tag::CodeBlock(kind)) => {
            in_code_block = true;
            let lang = match &kind {
                CodeBlockKind::Fenced(info) => info.split_whitespace().next().unwrap_or(""),
                CodeBlockKind::Indented => "",
            };
            let label = if lang.is_empty() { "code" } else { lang };
            Event::Html(CowStr::from(format!(
                "<div class=\"codeblock\"><div class=\"codeblock-head\"><span>{}</span>\
                 <button type=\"button\" data-copy-code>Copy</button></div><pre><code>",
                escape(label)
            )))
        }
        Event::End(TagEnd::CodeBlock) => {
            in_code_block = false;
            Event::Html(CowStr::from("</code></pre></div>"))
        }

        Event::Start(Tag::Link { link_type, dest_url, title, id }) => Event::Start(Tag::Link {
            link_type,
            dest_url: safe_url(dest_url),
            title,
            id,
        }),
        Event::Start(Tag::Image { link_type, dest_url, title, id }) => {
            in_image = true;
            Event::Start(Tag::Image { link_type, dest_url: safe_url(dest_url), title, id })
        }
        Event::End(TagEnd::Image) => {
            in_image = false;
            Event::End(TagEnd::Image)
        }

        other => other,
    });

    let mut out = String::with_capacity(src.len() * 3 / 2);
    html::push_html(&mut out, events);
    out
}

fn safe_url(url: CowStr<'_>) -> CowStr<'_> {
    let lower = url.trim_start().to_ascii_lowercase();
    let has_scheme = lower
        .split_once(':')
        .is_some_and(|(scheme, _)| !scheme.contains(['/', '?', '#']));
    let allowed = ["http:", "https:", "mailto:"].iter().any(|s| lower.starts_with(s));
    if has_scheme && !allowed { CowStr::from("#") } else { url }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::render;

    #[test]
    fn escapes_raw_html() {
        let out = render("<script>alert(1)</script> and <b>x</b>");
        assert!(!out.contains("<script>"), "{out}");
        assert!(!out.contains("<b>"), "{out}");
    }

    #[test]
    fn blocks_javascript_links() {
        let out = render("[click](javascript:alert(1)) [ok](https://example.com) [rel](/a:b)");
        assert!(!out.contains("javascript:"), "{out}");
        assert!(out.contains("https://example.com"), "{out}");
        assert!(out.contains("/a:b"), "{out}");
    }

    #[test]
    fn wraps_code_blocks_and_escapes_them() {
        let out = render("```rust\nlet x = \"<y>\";\n```");
        assert!(out.contains("<span>rust</span>"), "{out}");
        assert!(out.contains("data-copy-code"), "{out}");
        assert!(out.contains("&lt;y&gt;"), "{out}");
    }

    #[test]
    fn bold_wrapping_code_and_lone_asterisks() {
        let out = render("**Use `match` here** and 2 * 3 * 4");
        assert!(out.contains("<strong>Use <code>match</code> here</strong>"), "{out}");
        assert!(out.contains("2 * 3 * 4"), "{out}");
    }

    #[test]
    fn wraps_emoji_but_not_inside_code_or_raw_html() {
        let out = render("Hi 👍🏽 **🇵🇹** <i>😊</i>\n\n```\nlet s = \"🚀\";\n```\n`🔥` ![🐱](https://x/y.png)");
        assert!(out.contains("Hi <span class=\"emoji\">👍🏽</span>"), "{out}");
        assert!(out.contains("<strong><span class=\"emoji\">🇵🇹</span></strong>"), "{out}");
        assert!(out.contains("&lt;i&gt;<span class=\"emoji\">😊</span>&lt;/i&gt;"), "{out}");
        assert!(out.contains("let s = \"🚀\";"), "{out}");
        assert!(out.contains("<code>🔥</code>"), "{out}");
        assert!(out.contains("alt=\"🐱\""), "{out}");
    }

    #[test]
    fn unfinished_fence_while_streaming_is_fine() {
        let out = render("Here:\n```py\nprint('hi')");
        assert!(out.contains("print("), "{out}");
    }
}
