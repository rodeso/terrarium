//! The Newsstand module: a reader for RSS/Atom news feeds the user
//! subscribes to.

use crate::calculator::format_time;
use crate::chat::error_text;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::use_params_map;
use serde::{Deserialize, Serialize};

/// Feeds older than this are refreshed when the Newsstand opens.
pub const STALE_AFTER_MS: i64 = 20 * 60 * 1000;
/// Most articles shown in the list at once.
#[cfg_attr(not(feature = "ssr"), allow(dead_code))]
const LIST_LIMIT: i64 = 300;

/// Offered when adding a feed; nothing is subscribed automatically.
pub const SUGGESTED_FEEDS: [(&str, &str); 7] = [
    ("BBC News", "https://feeds.bbci.co.uk/news/rss.xml"),
    ("The Guardian · World", "https://www.theguardian.com/world/rss"),
    ("Hacker News", "https://hnrss.org/frontpage"),
    ("The Verge", "https://www.theverge.com/rss/index.xml"),
    ("Ars Technica", "https://feeds.arstechnica.com/arstechnica/index"),
    ("RTP Notícias", "https://www.rtp.pt/noticias/rss"),
    ("Observador", "https://observador.pt/feed/"),
];

// ======================================================================
// Types shared by server and browser
// ======================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedInfo {
    pub id: i64,
    pub title: String,
    pub url: String,
    pub site_url: Option<String>,
    pub unread: i64,
    pub last_error: Option<String>,
    pub fetched_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ArticleSummary {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    pub snippet: String,
    pub published_ms: i64,
    pub read: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Article {
    pub id: i64,
    pub feed_id: i64,
    pub feed_title: String,
    pub title: String,
    pub url: Option<String>,
    pub author: Option<String>,
    /// Sanitized on the server before it was stored.
    pub content_html: String,
    pub snippet: String,
    pub published_ms: i64,
    pub read: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NewsOverview {
    pub feeds: Vec<FeedInfo>,
    pub articles: Vec<ArticleSummary>,
    /// Some feed hasn't been fetched recently.
    pub stale: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RefreshReport {
    pub new_articles: usize,
    pub errors: Vec<String>,
}

// ======================================================================
// Server functions
// ======================================================================

#[server]
pub async fn news_list(feed: Option<i64>, #[server(default)] unread_only: bool) -> Result<NewsOverview, ServerFnError> {
    let state = crate::server::state().await?;
    let feeds = state.db.feeds().await.map_err(ServerFnError::new)?;
    let articles = state.db.articles(feed, unread_only, LIST_LIMIT).await.map_err(ServerFnError::new)?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
    let stale = feeds.iter().any(|f| f.fetched_ms.is_none_or(|t| now - t > STALE_AFTER_MS));
    Ok(NewsOverview { feeds, articles, stale })
}

/// Returns the article and marks it read.
#[server]
pub async fn open_article(id: i64) -> Result<Option<Article>, ServerFnError> {
    crate::server::state().await?.db.open_article(id).await.map_err(ServerFnError::new)
}

#[server]
pub async fn set_article_read(id: i64, read: bool) -> Result<(), ServerFnError> {
    crate::server::state().await?.db.set_article_read(id, read).await.map_err(ServerFnError::new)
}

#[server]
pub async fn mark_all_read(feed: Option<i64>) -> Result<(), ServerFnError> {
    crate::server::state().await?.db.mark_all_read(feed).await.map_err(ServerFnError::new)
}

/// Subscribes to a feed (or a site that links to one) and fetches it now.
#[server]
pub async fn add_feed(url: String) -> Result<i64, ServerFnError> {
    let state = crate::server::state().await?;
    let fetched = crate::feeds::fetch(&state.http, &url).await.map_err(ServerFnError::new)?;
    let id = state.db.upsert_feed(fetched.url, fetched.title.clone(), fetched.site_url).await.map_err(ServerFnError::new)?;
    state.db.store_fetch(id, Some(fetched.title), fetched.articles, None).await.map_err(ServerFnError::new)?;
    Ok(id)
}

#[server]
pub async fn remove_feed(id: i64) -> Result<(), ServerFnError> {
    crate::server::state().await?.db.remove_feed(id).await.map_err(ServerFnError::new)
}

/// Fetches every feed. Only one refresh runs at a time; a second request
/// waits for it and reports nothing new.
#[server]
pub async fn refresh_news() -> Result<RefreshReport, ServerFnError> {
    let state = crate::server::state().await?;
    let Ok(_guard) = state.news_refresh.try_lock() else {
        let _ = state.news_refresh.lock().await;
        return Ok(RefreshReport { new_articles: 0, errors: Vec::new() });
    };
    let (new_articles, errors) = crate::feeds::refresh_all(&state.db, &state.http).await.map_err(ServerFnError::new)?;
    Ok(RefreshReport { new_articles, errors })
}

// ======================================================================
// Page
// ======================================================================

#[derive(Clone, Copy)]
struct NewsCtx {
    articles: RwSignal<Vec<ArticleSummary>>,
    feeds: RwSignal<Vec<FeedInfo>>,
    mounted: RwSignal<bool>,
}

impl NewsCtx {
    /// Updates read state locally (list and unread counts). Does nothing if
    /// the Newsstand was closed meanwhile.
    fn mark_read(&self, id: i64, read: bool) {
        let mut feed_id = None;
        self.articles.try_update(|list| {
            if let Some(a) = list.iter_mut().find(|a| a.id == id) {
                if a.read != read {
                    a.read = read;
                    feed_id = Some(a.feed_id);
                }
            }
        });
        if let Some(f) = feed_id {
            self.feeds.try_update(|feeds| {
                if let Some(feed) = feeds.iter_mut().find(|x| x.id == f) {
                    feed.unread = (feed.unread + if read { -1 } else { 1 }).max(0);
                }
            });
        }
    }
}

#[component]
pub fn NewsPage() -> impl IntoView {
    let params = use_params_map();
    let selected = Memo::new(move |_| params.with(|p| p.get("id").and_then(|s| s.parse::<i64>().ok())));

    let feed_filter: RwSignal<Option<i64>> = RwSignal::new(None);
    let unread_only = RwSignal::new(false);
    let reload = RwSignal::new(0u32);
    let list = Resource::new(move || (feed_filter.get(), unread_only.get(), reload.get()), |(f, u, _)| news_list(f, u));

    let ctx = NewsCtx { articles: RwSignal::new(Vec::new()), feeds: RwSignal::new(Vec::new()), mounted: RwSignal::new(false) };
    provide_context(ctx);

    let refreshing = RwSignal::new(false);
    let message: RwSignal<Option<(bool, String)>> = RwSignal::new(None); // (is_error, text)
    let manage_open = RwSignal::new(false);

    let refresh = move || {
        if refreshing.get_untracked() {
            return;
        }
        refreshing.set(true);
        spawn_local(async move {
            match refresh_news().await {
                Ok(r) => {
                    let text = match r.new_articles {
                        0 => "No new articles".to_string(),
                        1 => "1 new article".to_string(),
                        n => format!("{n} new articles"),
                    };
                    if r.errors.is_empty() {
                        message.try_set(Some((false, text)));
                    } else {
                        message.try_set(Some((true, format!("{text}. Some feeds failed: {}", r.errors.join("; ")))));
                    }
                    reload.try_update(|n| *n += 1);
                }
                Err(e) => {
                    message.try_set(Some((true, error_text(e))));
                }
            }
            refreshing.try_set(false);
        });
    };

    // Refresh automatically when opening the Newsstand with stale feeds.
    let auto_refreshed = StoredValue::new(false);
    Effect::new(move |_| {
        ctx.mounted.set(true);
        if let Some(Ok(l)) = list.get() {
            if l.stale && !auto_refreshed.get_value() && !l.feeds.is_empty() {
                auto_refreshed.set_value(true);
                refresh();
            }
        }
    });

    let mark_all = move |_| {
        let feed = feed_filter.get_untracked();
        ctx.articles.update(|l| l.iter_mut().filter(|a| feed.is_none_or(|f| a.feed_id == f)).for_each(|a| a.read = true));
        ctx.feeds.update(|fs| fs.iter_mut().filter(|x| feed.is_none_or(|f| x.id == f)).for_each(|x| x.unread = 0));
        spawn_local(async move {
            if let Err(e) = mark_all_read(feed).await {
                message.try_set(Some((true, error_text(e))));
            }
        });
    };

    let total_unread = move || ctx.feeds.with(|f| f.iter().map(|x| x.unread).sum::<i64>());

    view! {
        <Title text="Newsstand · Workspace" />
        <section class="pane news-pane">
            <div class="toolbar news-toolbar">
                <button class="aero-btn" disabled=move || refreshing.get() on:click=move |_| refresh()>
                    <span class="btn-icon icon-refresh" class:spinning=move || refreshing.get() aria-hidden="true"></span>
                    {move || if refreshing.get() { "Refreshing…" } else { "Refresh" }}
                </button>
                <button class="aero-btn" class:pressed=move || manage_open.get() on:click=move |_| manage_open.update(|o| *o = !*o)>
                    <span class="btn-icon icon-feed" aria-hidden="true"></span>
                    "Feeds"
                </button>
                <select
                    class="aero-select"
                    aria-label="Show feed"
                    on:change=move |ev| {
                        let v = event_target_value(&ev);
                        feed_filter.set(v.parse::<i64>().ok());
                    }
                    prop:value=move || feed_filter.get().map(|f| f.to_string()).unwrap_or_default()
                >
                    <option value="">{move || format!("All feeds ({})", total_unread())}</option>
                    <For
                        each=move || ctx.feeds.get()
                        key=|f| (f.id, f.title.clone(), f.unread)
                        children=move |f| view! {
                            <option value=f.id.to_string()>{format!("{} ({})", f.title, f.unread)}</option>
                        }
                    />
                </select>
                <label class="check">
                    <input type="checkbox" prop:checked=move || unread_only.get() on:change=move |ev| unread_only.set(event_target_checked(&ev)) />
                    "Unread only"
                </label>
                <span class="spacer"></span>
                <span class="toolbar-note" class:error=move || message.with(|m| m.as_ref().is_some_and(|(e, _)| *e))>
                    {move || message.get().map(|(_, t)| t)}
                </span>
                <button class="aero-btn small" disabled=move || total_unread() == 0 on:click=mark_all>"Mark all read"</button>
            </div>

            <Show when=move || manage_open.get()>
                <FeedManager reload message on_close=move || manage_open.set(false) />
            </Show>

            <div class="news-layout" class:has-selection=move || selected.get().is_some()>
                <aside class="news-list" aria-label="Articles">
                    <Suspense fallback=|| view! { <p class="notes-empty">"Loading…"</p> }>
                        {move || Suspend::new(async move {
                            match list.await {
                                Ok(l) => {
                                    ctx.feeds.set(l.feeds);
                                    ctx.articles.set(l.articles);
                                }
                                Err(e) => return view! { <p class="notes-empty">{error_text(e)}</p> }.into_any(),
                            }
                            view! {
                                <Show
                                    when=move || ctx.articles.with(|a| !a.is_empty())
                                    fallback=move || view! {
                                        <div class="news-empty">
                                            {move || if ctx.feeds.with(Vec::is_empty) {
                                                view! {
                                                    <div class="news-hero" aria-hidden="true"></div>
                                                    <h2>"Your newsstand is empty"</h2>
                                                    <p>"Add a few news sources to get started."</p>
                                                    <button class="aero-btn" on:click=move |_| manage_open.set(true)>"Add feeds"</button>
                                                }.into_any()
                                            } else if unread_only.get() {
                                                view! { <h2>"All caught up"</h2><p>"No unread articles."</p> }.into_any()
                                            } else {
                                                view! { <h2>"No articles yet"</h2><p>"Try Refresh."</p> }.into_any()
                                            }}
                                        </div>
                                    }
                                >
                                    <ul class="notes-items">
                                        <For
                                            each=move || ctx.articles.get()
                                            key=|a| (a.id, a.read)
                                            children=move |a| view! { <ArticleItem article=a selected /> }
                                        />
                                    </ul>
                                </Show>
                            }
                            .into_any()
                        })}
                    </Suspense>
                </aside>

                <div class="news-reader-area">
                    {move || match selected.get() {
                        Some(id) => view! { <ArticleReader id /> }.into_any(),
                        None => view! {
                            <div class="notes-placeholder">
                                <div class="news-hero" aria-hidden="true"></div>
                                <h2>"Newsstand"</h2>
                                <p>"Pick an article to read it here."</p>
                            </div>
                        }
                        .into_any(),
                    }}
                </div>
            </div>
        </section>
    }
}

#[component]
fn ArticleItem(article: ArticleSummary, selected: Memo<Option<i64>>) -> impl IntoView {
    let ctx = expect_context::<NewsCtx>();
    let id = article.id;
    let at = article.published_ms;
    view! {
        <li>
            <a
                class="note-item news-item"
                class:active=move || selected.get() == Some(id)
                class:unread=!article.read
                aria-current=move || (selected.get() == Some(id)).then_some("page")
                href=format!("/news/{id}")
            >
                <div class="note-item-title">
                    {(!article.read).then(|| view! { <span class="unread-dot" aria-label="Unread"></span> })}
                    <span>{article.title.clone()}</span>
                </div>
                <div class="news-item-meta">
                    <span class="news-source">{article.feed_title.clone()}</span>
                    <span>{move || ctx.mounted.get().then(|| format_time(at as u64))}</span>
                </div>
                <div class="note-item-snippet">{article.snippet.clone()}</div>
            </a>
        </li>
    }
}

#[component]
fn ArticleReader(id: i64) -> impl IntoView {
    let ctx = expect_context::<NewsCtx>();
    let article = Resource::new(move || id, open_article);
    view! {
        <Suspense fallback=|| view! { <div class="notes-placeholder"><p>"Opening…"</p></div> }>
            {move || Suspend::new(async move {
                match article.await {
                    Ok(Some(a)) => {
                        ctx.mark_read(a.id, true);
                        view! { <Reader article=a /> }.into_any()
                    }
                    Ok(None) => view! {
                        <div class="notes-placeholder">
                            <h2>"Article not found"</h2>
                            <p>"It may have been removed with its feed."</p>
                            <a href="/news">"Back to the newsstand"</a>
                        </div>
                    }
                    .into_any(),
                    Err(e) => view! { <div class="notes-placeholder"><p>{error_text(e)}</p></div> }.into_any(),
                }
            })}
        </Suspense>
    }
}

#[component]
fn Reader(article: Article) -> impl IntoView {
    let ctx = expect_context::<NewsCtx>();
    let id = article.id;
    let at = article.published_ms;
    let is_read = RwSignal::new(true);
    let toggle_read = move |_| {
        let next = !is_read.get_untracked();
        is_read.set(next);
        ctx.mark_read(id, next);
        spawn_local(async move {
            let _ = set_article_read(id, next).await;
        });
    };
    let body = if article.content_html.trim().is_empty() {
        format!("<p>{}</p>", escape(&article.snippet))
    } else {
        article.content_html.clone()
    };
    view! {
        <article class="reader">
            <div class="note-bar">
                <a class="aero-btn small back-link" href="/news">"‹ News"</a>
                <span class="news-source">{article.feed_title.clone()}</span>
                <span class="spacer"></span>
                <button type="button" class="aero-btn small" on:click=toggle_read>
                    {move || if is_read.get() { "Mark unread" } else { "Mark read" }}
                </button>
                {article.url.clone().map(|url| view! {
                    <a class="aero-btn small" href=url target="_blank" rel="noopener noreferrer">"Open original ↗"</a>
                })}
            </div>
            <div class="reader-scroll">
                <h1 class="reader-title">{article.title.clone()}</h1>
                <div class="reader-meta">
                    {article.author.clone().map(|a| view! { <span>{a}</span><span class="dot">"·"</span> })}
                    <span>{move || ctx.mounted.get().then(|| format_time(at as u64))}</span>
                </div>
                // Sanitized with ammonia on the server when the feed was fetched.
                <div class="reader-body md" inner_html=body></div>
                {article.url.map(|url| view! {
                    <p class="reader-more">
                        <a href=url target="_blank" rel="noopener noreferrer">"Continue reading on the original site ↗"</a>
                    </p>
                })}
            </div>
        </article>
    }
}

/// Add and remove feeds, with one-click suggestions.
#[component]
fn FeedManager(
    reload: RwSignal<u32>,
    message: RwSignal<Option<(bool, String)>>,
    on_close: impl Fn() + Copy + Send + Sync + 'static,
) -> impl IntoView {
    let ctx = expect_context::<NewsCtx>();
    let input = RwSignal::new(String::new());
    let adding: RwSignal<Option<String>> = RwSignal::new(None); // URL being added
    let error: RwSignal<Option<String>> = RwSignal::new(None);

    let add = move |url: String| {
        if adding.get_untracked().is_some() || url.trim().is_empty() {
            return;
        }
        adding.set(Some(url.clone()));
        error.set(None);
        spawn_local(async move {
            match add_feed(url).await {
                Ok(_) => {
                    input.try_set(String::new());
                    message.try_set(Some((false, "Feed added".into())));
                    reload.try_update(|n| *n += 1);
                }
                Err(e) => {
                    error.try_set(Some(error_text(e)));
                }
            }
            adding.try_set(None);
        });
    };

    let remove = move |id: i64, title: String| {
        if !window().confirm_with_message(&format!("Remove “{title}” and its articles?")).unwrap_or(false) {
            return;
        }
        ctx.feeds.update(|f| f.retain(|x| x.id != id));
        ctx.articles.update(|a| a.retain(|x| x.feed_id != id));
        spawn_local(async move {
            if let Err(e) = remove_feed(id).await {
                message.try_set(Some((true, error_text(e))));
            }
            reload.try_update(|n| *n += 1);
        });
    };

    view! {
        <div class="feed-manager">
            <form class="feed-add" on:submit=move |ev| {
                ev.prevent_default();
                add(input.get_untracked());
            }>
                <input
                    type="url"
                    inputmode="url"
                    placeholder="Feed or website address, e.g. bbc.co.uk/news"
                    aria-label="Feed or website address"
                    prop:value=move || input.get()
                    on:input=move |ev| input.set(event_target_value(&ev))
                />
                <button type="submit" class="aero-btn" disabled=move || adding.get().is_some()>
                    {move || if adding.get().is_some() { "Adding…" } else { "Add" }}
                </button>
                <button type="button" class="aero-btn small" on:click=move |_| on_close()>"Done"</button>
            </form>
            {move || error.get().map(|e| view! { <p class="feed-error" role="alert">{e}</p> })}

            <div class="feed-columns">
                <div>
                    <h3>"Your feeds"</h3>
                    <Show when=move || ctx.feeds.with(|f| !f.is_empty()) fallback=|| view! { <p class="muted">"None yet."</p> }>
                        <ul class="feed-rows">
                            <For
                                each=move || ctx.feeds.get()
                                key=|f| (f.id, f.title.clone(), f.last_error.clone())
                                children=move |f| {
                                    let (id, title) = (f.id, f.title.clone());
                                    view! {
                                        <li>
                                            <span class="feed-name" title=f.url.clone()>{f.title.clone()}</span>
                                            {f.last_error.clone().map(|e| view! { <span class="feed-warn" title=e>"⚠ failed last time"</span> })}
                                            <button type="button" class="link-btn" on:click=move |_| remove(id, title.clone())>"Remove"</button>
                                        </li>
                                    }
                                }
                            />
                        </ul>
                    </Show>
                </div>
                <div>
                    <h3>"Suggestions"</h3>
                    <div class="suggested-feeds">
                        {SUGGESTED_FEEDS
                            .iter()
                            .map(|&(name, url)| {
                                let subscribed = move || ctx.feeds.with(|f| f.iter().any(|x| x.url.trim_end_matches('/') == url.trim_end_matches('/')));
                                view! {
                                    <button
                                        type="button"
                                        class="command-link small"
                                        disabled=move || subscribed() || adding.get().is_some()
                                        on:click=move |_| add(url.to_string())
                                    >
                                        <span class="arrow" aria-hidden="true">{move || if subscribed() { "✓" } else { "+" }}</span>
                                        <span>
                                            {name}
                                            {move || (adding.get().as_deref() == Some(url)).then_some(" (adding…)")}
                                        </span>
                                    </button>
                                }
                            })
                            .collect_view()}
                    </div>
                </div>
            </div>
        </div>
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}
