//! Server-only: fetching, parsing and cleaning news feeds (RSS, Atom and
//! JSON Feed), and refreshing every subscribed feed.

use crate::db::{Db, NewArticle};
use ammonia::Url;
use futures::StreamExt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const USER_AGENT: &str = concat!("Terrarium/", env!("CARGO_PKG_VERSION"), " (personal feed reader)");
const TIMEOUT: Duration = Duration::from_secs(20);
/// Feeds bigger than this are refused.
const MAX_BYTES: usize = 5 * 1024 * 1024;
/// Most entries read from one fetch (feeds list newest first).
const MAX_ENTRIES: usize = 100;
/// Feeds fetched at the same time during a refresh.
const CONCURRENT_FETCHES: usize = 4;
const SNIPPET_CHARS: usize = 220;

pub struct FetchedFeed {
    /// The feed's own URL (after discovery, if a web page was given).
    pub url: String,
    pub title: String,
    pub site_url: Option<String>,
    pub articles: Vec<NewArticle>,
}

pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(TIMEOUT)
        .build()
        .expect("HTTP client builds")
}

/// Fetches and parses a feed. If `input` is a web page rather than a feed,
/// follows the page's `<link rel="alternate">` to its feed.
pub async fn fetch(client: &reqwest::Client, input: &str) -> Result<FetchedFeed, String> {
    let url = normalize_url(input)?;
    let (body, final_url) = download(client, &url).await?;
    match parse(&body, &final_url) {
        Ok(feed) => Ok(feed),
        Err(parse_error) => {
            let Some(found) = discover_feed_link(&String::from_utf8_lossy(&body), &final_url) else {
                return Err(if looks_like_html(&body) {
                    "That page isn't a feed, and it doesn't link to one. Try the site's RSS or Atom address.".into()
                } else {
                    format!("Couldn't read that feed: {parse_error}")
                });
            };
            let (body, final_url) = download(client, &found).await?;
            parse(&body, &final_url).map_err(|e| format!("Couldn't read the feed at {found}: {e}"))
        }
    }
}

/// Refreshes every subscribed feed. Returns (new articles, error messages).
pub async fn refresh_all(db: &Db, client: &reqwest::Client) -> Result<(usize, Vec<String>), String> {
    let feeds = db.feeds().await?;
    let results = futures::stream::iter(feeds)
        .map(|feed| async move {
            let outcome = fetch(client, &feed.url).await;
            (feed, outcome)
        })
        .buffer_unordered(CONCURRENT_FETCHES)
        .collect::<Vec<_>>()
        .await;

    let mut added = 0;
    let mut errors = Vec::new();
    for (feed, outcome) in results {
        match outcome {
            Ok(fetched) => added += db.store_fetch(feed.id, Some(fetched.title), fetched.articles, None).await?,
            Err(e) => {
                errors.push(format!("{}: {e}", if feed.title.is_empty() { &feed.url } else { &feed.title }));
                db.store_fetch(feed.id, None, Vec::new(), Some(e)).await?;
            }
        }
    }
    Ok((added, errors))
}

fn normalize_url(input: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("Enter a feed or website address".into());
    }
    let with_scheme = if input.contains("://") { input.to_string() } else { format!("https://{input}") };
    let url = Url::parse(&with_scheme).map_err(|_| format!("“{input}” isn't a valid web address"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("Only http and https addresses are supported".into());
    }
    Ok(url.to_string())
}

async fn download(client: &reqwest::Client, url: &str) -> Result<(Vec<u8>, Url), String> {
    let response = client
        .get(url)
        .header("Accept", "application/rss+xml, application/atom+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.9, */*;q=0.5")
        .send()
        .await
        .map_err(|e| {
            if e.is_timeout() {
                "The site took too long to respond".to_string()
            } else if e.is_connect() {
                let host = e.url().and_then(|u| u.host_str()).unwrap_or("the site");
                format!("Couldn't connect to {host}. Check the address and your internet connection.")
            } else {
                format!("Couldn't reach the site: {}", root_cause(&e))
            }
        })?;
    if !response.status().is_success() {
        return Err(format!("The site answered {}", response.status()));
    }
    if response.content_length().is_some_and(|n| n as usize > MAX_BYTES) {
        return Err("The feed is too large".into());
    }
    let final_url = response.url().clone();
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download failed: {}", root_cause(&e)))?;
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BYTES {
            return Err("The feed is too large".into());
        }
    }
    Ok((body, final_url))
}

fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut current = e;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}

fn parse(body: &[u8], url: &Url) -> Result<FetchedFeed, String> {
    let feed = feed_rs::parser::parse(body).map_err(|e| e.to_string())?;
    let site_url = feed.links.iter().map(|l| l.href.clone()).find(|h| !h.ends_with(".xml") && h != url.as_str());
    let base = site_url.as_deref().and_then(|s| Url::parse(s).ok()).unwrap_or_else(|| url.clone());
    let title = feed
        .title
        .map(|t| html_to_text(&t.content))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| url.host_str().unwrap_or("Feed").to_string());

    let now = now_ms();
    let articles = feed
        .entries
        .into_iter()
        .take(MAX_ENTRIES)
        .map(|entry| {
            let link = entry.links.first().map(|l| l.href.clone());
            let raw_html = entry
                .content
                .and_then(|c| c.body)
                .or_else(|| entry.summary.as_ref().map(|s| s.content.clone()))
                .unwrap_or_default();
            let entry_base = link.as_deref().and_then(|l| base.join(l).ok()).unwrap_or_else(|| base.clone());
            let content_html = sanitize(&raw_html, &entry_base);
            let text = html_to_text(&raw_html);
            let title = entry.title.map(|t| html_to_text(&t.content)).filter(|t| !t.is_empty());
            NewArticle {
                guid: if entry.id.is_empty() { link.clone().unwrap_or_default() } else { entry.id },
                url: link.and_then(|l| base.join(&l).ok()).map(|u| u.to_string()),
                title: title.unwrap_or_else(|| truncate(&text, 80)),
                snippet: truncate(&text, SNIPPET_CHARS),
                content_html,
                author: entry.authors.into_iter().find_map(|a| a.name).map(|n| html_to_text(&n)).filter(|n| !n.is_empty()),
                published_ms: entry
                    .published
                    .or(entry.updated)
                    .map(|d| d.timestamp_millis())
                    .filter(|&ms| ms <= now + 86_400_000) // ignore dates far in the future
                    .unwrap_or(now),
            }
        })
        .collect();

    Ok(FetchedFeed { url: url.to_string(), title, site_url, articles })
}

/// Cleans feed HTML for display: no scripts, styles, forms or event
/// handlers; links open in a new tab; relative links and images resolved.
fn sanitize(html: &str, base: &Url) -> String {
    ammonia::Builder::default()
        .url_relative(ammonia::UrlRelative::RewriteWithBase(base.clone()))
        .link_rel(Some("noopener noreferrer"))
        .set_tag_attribute_value("a", "target", "_blank")
        .set_tag_attribute_value("img", "loading", "lazy")
        .clean(html)
        .to_string()
}

/// Plain text from HTML: tags removed, common entities decoded, whitespace
/// collapsed.
pub fn html_to_text(html: &str) -> String {
    let mut text = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                text.push(' ');
            }
            '>' if in_tag => in_tag = false,
            c if !in_tag => text.push(c),
            _ => {}
        }
    }
    let decoded = decode_entities(&text);
    decoded.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        // Look for the closing ';' within a few characters (not bytes:
        // slicing bytes could split a multi-byte character like "ç").
        let Some(end) = after.char_indices().take(12).find(|&(_, c)| c == ';').map(|(i, _)| i) else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            "hellip" => Some('…'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            "lsquo" => Some('‘'),
            "rsquo" => Some('’'),
            "ldquo" => Some('“'),
            "rdquo" => Some('”'),
            e if e.starts_with("#x") || e.starts_with("#X") => u32::from_str_radix(&e[2..], 16).ok().and_then(char::from_u32),
            e if e.starts_with('#') => e[1..].parse::<u32>().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[end + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Finds a page's `<link rel="alternate" type="application/rss+xml" href="…">`.
fn discover_feed_link(html: &str, page: &Url) -> Option<String> {
    // ASCII lowercasing keeps byte positions identical to `html`.
    let lower = html.to_ascii_lowercase();
    let mut from = 0;
    while let Some(pos) = lower[from..].find("<link") {
        let start = from + pos;
        let end = lower[start..].find('>').map(|e| start + e)?;
        let tag = &lower[start..end];
        let is_feed = tag.contains("alternate")
            && (tag.contains("rss+xml") || tag.contains("atom+xml") || tag.contains("feed+json"));
        if is_feed {
            if let Some(href) = attribute(&html[start..end], "href") {
                return page.join(&decode_entities(&href)).ok().map(|u| u.to_string());
            }
        }
        from = end;
    }
    None
}

/// The value of `name="…"` (or single-quoted) in a tag.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find(name) {
        let at = search + pos;
        let before_ok = at == 0 || lower.as_bytes()[at - 1].is_ascii_whitespace();
        let rest = tag[at + name.len()..].trim_start();
        if before_ok && rest.starts_with('=') {
            let value = rest[1..].trim_start();
            let quote = value.chars().next()?;
            if quote == '"' || quote == '\'' {
                return value[1..].find(quote).map(|e| value[1..1 + e].to_string());
            }
            return Some(value.split(|c: char| c.is_whitespace() || c == '/').next()?.to_string());
        }
        search = at + name.len();
    }
    None
}

fn looks_like_html(body: &[u8]) -> bool {
    let head = String::from_utf8_lossy(&body[..body.len().min(2048)]).to_lowercase();
    head.contains("<html") || head.contains("<!doctype html")
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>().trim_end())
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_from_html() {
        assert_eq!(html_to_text("<p>Tom &amp; Jerry&#8217;s <b>big</b>\n day&hellip;</p>"), "Tom & Jerry’s big day…");
        assert_eq!(html_to_text("5 &lt; 6 &unknown; &"), "5 < 6 &unknown; &");
        // Multi-byte characters right after '&' (this used to panic on RTP's feed).
        assert_eq!(html_to_text("Saúde & educação: ações já"), "Saúde & educação: ações já");
        assert_eq!(html_to_text("&ççççççççççççç;"), "&ççççççççççççç;");
    }

    #[test]
    fn sanitizes_dangerous_html() {
        let base = Url::parse("https://example.com/news/").unwrap();
        let out = sanitize(
            r#"<p onclick="x()">Hi<script>alert(1)</script></p><a href="/a">a</a><img src="pic.jpg"><a href="javascript:alert(1)">j</a><style>p{}</style>"#,
            &base,
        );
        assert!(!out.contains("script") && !out.contains("onclick") && !out.contains("javascript:") && !out.contains("<style"), "{out}");
        assert!(out.contains(r#"href="https://example.com/a""#), "{out}");
        assert!(out.contains(r#"src="https://example.com/news/pic.jpg""#), "{out}");
        assert!(out.contains(r#"target="_blank""#) && out.contains("noopener"), "{out}");
    }

    #[test]
    fn finds_feed_link_in_page() {
        let page = Url::parse("https://example.com/blog/").unwrap();
        let html = r#"<html><head><link rel="stylesheet" href="s.css"><LINK type="application/rss+xml" rel="alternate" title="Feed" href="/feed.xml?a=1&amp;b=2"></head>"#;
        assert_eq!(discover_feed_link(html, &page).as_deref(), Some("https://example.com/feed.xml?a=1&b=2"));
        assert_eq!(discover_feed_link("<html><link rel=icon href=x.png></html>", &page), None);
        // Non-ASCII before the tag mustn't shift positions.
        let html = r#"<title>İstanbul Notícias</title><link rel="alternate" type="application/atom+xml" href="/atom">"#;
        assert_eq!(discover_feed_link(html, &page).as_deref(), Some("https://example.com/atom"));
    }

    #[test]
    fn parses_rss_and_atom() {
        let rss = br#"<?xml version="1.0"?><rss version="2.0"><channel><title>My &amp; Feed</title><link>https://site.example/</link>
            <item><title>First</title><link>/post/1</link><guid>p1</guid><description>&lt;p&gt;Hello &lt;b&gt;world&lt;/b&gt;&lt;/p&gt;</description><pubDate>Mon, 09 Nov 2026 10:00:00 GMT</pubDate></item>
            </channel></rss>"#;
        let f = parse(rss, &Url::parse("https://site.example/rss").unwrap()).unwrap();
        assert_eq!(f.title, "My & Feed");
        let a = &f.articles[0];
        assert_eq!((a.title.as_str(), a.guid.as_str(), a.snippet.as_str()), ("First", "p1", "Hello world"));
        assert_eq!(a.url.as_deref(), Some("https://site.example/post/1"));
        assert!(a.content_html.contains("<b>world</b>"));
        assert_eq!(a.published_ms, 1_794_218_400_000);

        let atom = br#"<?xml version="1.0"?><feed xmlns="http://www.w3.org/2005/Atom"><title>Atom</title>
            <entry><id>tag:1</id><title>Entry</title><link href="https://a.example/e"/><updated>2026-11-08T12:00:00Z</updated>
            <author><name>Ana</name></author><content type="html">Body</content></entry></feed>"#;
        let f = parse(atom, &Url::parse("https://a.example/atom").unwrap()).unwrap();
        assert_eq!(f.articles[0].author.as_deref(), Some("Ana"));
        assert!(parse(b"<html>not a feed</html>", &Url::parse("https://a.example/").unwrap()).is_err());
    }

    #[test]
    fn urls_are_normalized() {
        assert_eq!(normalize_url("bbc.co.uk").unwrap(), "https://bbc.co.uk/");
        assert!(normalize_url("ftp://x.example").is_err());
        assert!(normalize_url("  ").is_err());
    }
}
