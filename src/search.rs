//! The Search module: web search through your own SearXNG, shown as a tab
//! and also offered to the model as the `web_search` tool.

use crate::chat::error_text;
use leptos::prelude::*;
use leptos_meta::Title;
use leptos_router::hooks::{use_navigate, use_query_map};
use serde::{Deserialize, Serialize};

// ======================================================================
// Types shared by server and browser
// ======================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub query: String,
    pub results: Vec<SearchResult>,
    /// A short direct answer some engines give (e.g. from Wikipedia).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<SearchAnswer>,
    /// Related searches.
    #[serde(default)]
    pub suggestions: Vec<String>,
    /// Engines that failed, with the reason: "brave (too many requests)".
    #[serde(default)]
    pub unresponsive: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchResult {
    pub title: String,
    /// Always http(s).
    pub url: String,
    /// "rust-lang.org/learn"
    pub display_url: String,
    pub snippet: String,
    pub engines: Vec<String>,
    /// "2026-10-13"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SearchAnswer {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// "/search?q=rust%20book": a link to the Search tab with this query.
pub fn link(query: &str) -> String {
    let mut out = String::from("/search?q=");
    for b in query.trim().bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Longest query accepted.
#[cfg_attr(not(feature = "ssr"), allow(dead_code))]
const MAX_QUERY_CHARS: usize = 400;

// ======================================================================
// Server functions
// ======================================================================

#[server]
pub async fn search_web(query: String) -> Result<SearchResponse, ServerFnError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(ServerFnError::new("Type something to search for."));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(ServerFnError::new("That search is too long."));
    }
    let state = crate::server::state().await?;
    crate::websearch::search(&state.http, &state.search_url, query).await.map_err(ServerFnError::new)
}

// ======================================================================
// Components
// ======================================================================

#[component]
pub fn SearchPage() -> impl IntoView {
    let query_map = use_query_map();
    let current = move || query_map.with(|m| m.get("q").map(|q| q.trim().to_string()).filter(|q| !q.is_empty()));
    let text = RwSignal::new(current().unwrap_or_default());
    // Follow the address when it changes (back/forward, links from chat).
    Effect::new(move |_| text.set(current().unwrap_or_default()));

    let results = Resource::new(current, |q| async move {
        match q {
            Some(q) => search_web(q).await.map(Some),
            None => Ok(None),
        }
    });

    let navigate = use_navigate();
    let submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        let q = text.get_untracked().trim().to_string();
        if q.is_empty() {
            return;
        }
        if current().as_deref() == Some(q.as_str()) {
            results.refetch(); // same search again
        } else {
            navigate(&link(&q), Default::default());
        }
    };

    view! {
        <Title text=move || match current() {
            Some(q) => format!("{q} · Search · Terrarium"),
            None => "Search · Terrarium".to_string(),
        } />
        <section class="pane search-pane">
            <div class="toolbar">
                <span class="toolbar-title">"Web search"</span>
                <span class="spacer"></span>
                <span class="toolbar-note">"Through your own SearXNG, without an account or cookies"</span>
            </div>
            <div class="search-scroll">
                <div class="search-wrap">
                    <form class="search-form" role="search" on:submit=submit>
                        <span class="search-field-icon" aria-hidden="true"></span>
                        <input
                            type="search"
                            name="q"
                            placeholder="Search the web"
                            aria-label="Search the web"
                            autocomplete="off"
                            autofocus=move || current().is_none()
                            prop:value=move || text.get()
                            on:input=move |ev| text.set(event_target_value(&ev))
                        />
                        <button class="aero-btn" type="submit">"Search"</button>
                    </form>
                    <Suspense fallback=|| view! { <p class="search-status">"Searching…"</p> }>
                        {move || Suspend::new(async move {
                            match results.await {
                                Ok(Some(response)) => view! { <SearchResults response /> }.into_any(),
                                Ok(None) => view! { <SearchIntro /> }.into_any(),
                                Err(e) => view! {
                                    <div class="search-error" role="alert">
                                        <strong>"Search failed"</strong>
                                        <p>{error_text(e)}</p>
                                    </div>
                                }
                                .into_any(),
                            }
                        })}
                    </Suspense>
                </div>
            </div>
        </section>
    }
}

#[component]
fn SearchIntro() -> impl IntoView {
    view! {
        <div class="search-intro">
            <span class="search-hero" aria-hidden="true"></span>
            <h2>"Search the web"</h2>
            <p>"Results come from your own SearXNG. The assistant can search too, when a question needs it."</p>
        </div>
    }
}

#[component]
fn SearchResults(response: SearchResponse) -> impl IntoView {
    let SearchResponse { results, answer, suggestions, unresponsive, .. } = response;
    let count = match results.len() {
        0 => "No results".to_string(),
        1 => "1 result".to_string(),
        n => format!("{n} results"),
    };
    view! {
        <p class="search-status">
            {count}
            {(!unresponsive.is_empty()).then(|| view! {
                <span class="search-skipped">" · didn't respond: " {unresponsive.join(", ")}</span>
            })}
        </p>

        {answer.map(|a| view! {
            <div class="search-answer">
                <p>{a.text}</p>
                {a.url.map(|u| view! {
                    <a href=u target="_blank" rel="noopener noreferrer">"Source"</a>
                })}
            </div>
        })}

        <ol class="search-results">
            {results
                .into_iter()
                .map(|r| {
                    let meta = {
                        let mut parts = Vec::new();
                        if let Some(d) = &r.published {
                            parts.push(d.clone());
                        }
                        if !r.engines.is_empty() {
                            parts.push(r.engines.join(", "));
                        }
                        parts.join(" · ")
                    };
                    view! {
                        <li class="result">
                            <a class="result-title" href=r.url.clone() target="_blank" rel="noopener noreferrer">{r.title}</a>
                            <div class="result-url">{r.display_url}</div>
                            {(!r.snippet.is_empty()).then(|| view! { <p class="result-snippet">{r.snippet}</p> })}
                            {(!meta.is_empty()).then(|| view! { <div class="result-meta">{meta}</div> })}
                        </li>
                    }
                })
                .collect_view()}
        </ol>

        {(!suggestions.is_empty()).then(|| view! {
            <div class="search-suggestions">
                <span>"Related:"</span>
                {suggestions
                    .into_iter()
                    .map(|s| {
                        let href = link(&s);
                        view! { <a href=href>{s}</a> }
                    })
                    .collect_view()}
            </div>
        })}
    }
}

#[cfg(test)]
mod tests {
    use super::link;

    #[test]
    fn links_encode_the_query() {
        assert_eq!(link("rust & wasm?"), "/search?q=rust%20%26%20wasm%3F");
        assert_eq!(link("  café  "), "/search?q=caf%C3%A9");
    }
}
