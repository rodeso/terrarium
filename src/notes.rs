//! The Notes module: markdown notes saved in the workspace database, with
//! search, pinning, autosave and a live preview.

use crate::calculator::format_time;
use crate::chat::error_text;
use crate::markdown;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::{use_navigate, use_params_map};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Pause in typing before an automatic save.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(700);
/// Pause in typing before searching.
const SEARCH_DELAY: Duration = Duration::from_millis(250);

// ======================================================================
// Types shared by server and browser
// ======================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoteSummary {
    pub id: i64,
    pub title: String,
    pub snippet: String,
    pub pinned: bool,
    pub updated_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Note {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub pinned: bool,
    pub created_ms: i64,
    pub updated_ms: i64,
}

/// A one-line preview of a note body: markdown symbols removed, whitespace
/// collapsed, at most 120 characters.
pub fn snippet(body: &str) -> String {
    let text = body
        .lines()
        .map(|l| l.trim_start_matches(|c: char| matches!(c, '#' | '>' | '-' | '*' | '+' | ' ' | '\t')))
        .filter(|l| !l.starts_with("```"))
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['*', '_', '`'], "");
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() > 120 {
        format!("{}…", collapsed.chars().take(119).collect::<String>().trim_end())
    } else {
        collapsed
    }
}

/// Words too common to help a search (English and Portuguese).
const STOP_WORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "what", "which", "about", "from", "have", "has",
    "any", "did", "does", "are", "was", "were", "you", "your", "note", "notes", "write", "wrote",
    "written", "find", "show", "tell", "uma", "que", "para", "por", "com", "sobre", "nota",
    "notas", "minha", "minhas", "meu", "meus", "dos", "das", "nas", "nos",
];

/// Turns a search into loose terms: lowercase, common words dropped, and
/// long words cut to a stem so "grocery" also finds "groceries".
pub fn search_terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|w| w.chars().count() > 2 && !STOP_WORDS.contains(&w.as_str()))
        .map(|w| {
            let n = w.chars().count();
            if n >= 6 { w.chars().take(n - 2).collect() } else { w }
        })
        .collect();
    terms.sort();
    terms.dedup();
    terms
}

/// How well a note matches: 3 points per term found in the title, 1 per
/// term found in the body. Case-insensitive for all scripts.
pub fn relevance(terms: &[String], title: &str, body: &str) -> u32 {
    let (title, body) = (title.to_lowercase(), body.to_lowercase());
    terms
        .iter()
        .map(|t| if title.contains(t.as_str()) { 3 } else if body.contains(t.as_str()) { 1 } else { 0 })
        .sum()
}

fn display_title(title: &str) -> String {
    if title.trim().is_empty() { "Untitled".into() } else { title.trim().to_string() }
}

// ======================================================================
// Server functions
// ======================================================================

#[server]
pub async fn list_notes(query: String) -> Result<Vec<NoteSummary>, ServerFnError> {
    crate::server::state().await?.db.list_notes(query).await.map_err(ServerFnError::new)
}

#[server]
pub async fn get_note(id: i64) -> Result<Option<Note>, ServerFnError> {
    crate::server::state().await?.db.get_note(id).await.map_err(ServerFnError::new)
}

#[server]
pub async fn create_note() -> Result<Note, ServerFnError> {
    crate::server::state().await?.db.create_note().await.map_err(ServerFnError::new)
}

/// Returns the note's new `updated_ms`.
#[server]
pub async fn save_note(id: i64, title: String, body: String) -> Result<i64, ServerFnError> {
    crate::server::state()
        .await?
        .db
        .save_note(id, title, body)
        .await
        .map_err(ServerFnError::new)?
        .ok_or_else(|| ServerFnError::new("This note was deleted"))
}

#[server]
pub async fn pin_note(id: i64, pinned: bool) -> Result<(), ServerFnError> {
    crate::server::state().await?.db.set_pinned(id, pinned).await.map_err(ServerFnError::new)
}

#[server]
pub async fn delete_note(id: i64) -> Result<(), ServerFnError> {
    crate::server::state().await?.db.delete_note(id).await.map_err(ServerFnError::new)
}

// ======================================================================
// Page
// ======================================================================

/// Shared between the list and the editor.
#[derive(Clone, Copy)]
struct NotesCtx {
    /// The list as last loaded, updated locally after edits.
    notes: RwSignal<Vec<NoteSummary>>,
    mounted: RwSignal<bool>,
}

impl NotesCtx {
    /// Applies a local change to one summary and keeps the list ordered
    /// (pinned first, then most recently edited).
    /// Does nothing if the Notes tab has been closed meanwhile.
    fn update(&self, id: i64, f: impl FnOnce(&mut NoteSummary)) {
        self.notes.try_update(|list| {
            if let Some(n) = list.iter_mut().find(|n| n.id == id) {
                f(n);
            }
            list.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(b.updated_ms.cmp(&a.updated_ms)));
        });
    }
}

#[component]
pub fn NotesPage() -> impl IntoView {
    let params = use_params_map();
    let selected = Memo::new(move |_| params.with(|p| p.get("id").and_then(|s| s.parse::<i64>().ok())));

    let query = RwSignal::new(String::new());
    let search = RwSignal::new(String::new()); // `query`, after a pause in typing
    let list = Resource::new(move || search.get(), list_notes);

    let ctx = NotesCtx { notes: RwSignal::new(Vec::new()), mounted: RwSignal::new(false) };
    provide_context(ctx);
    Effect::new(move |_| ctx.mounted.set(true));

    let search_gen = StoredValue::new(0u64);
    let on_search = move |ev| {
        query.set(event_target_value(&ev));
        search_gen.update_value(|g| *g += 1);
        let this = search_gen.get_value();
        set_timeout(
            move || {
                if search_gen.get_value() == this {
                    search.set(query.get_untracked());
                }
            },
            SEARCH_DELAY,
        );
    };

    let navigate = use_navigate();
    let creating = RwSignal::new(false);
    let new_note = move |_| {
        if creating.get_untracked() {
            return;
        }
        creating.set(true);
        let navigate = navigate.clone();
        spawn_local(async move {
            match create_note().await {
                Ok(note) => {
                    query.set(String::new());
                    search.set(String::new());
                    ctx.notes.update(|l| {
                        let at = l.iter().position(|n| !n.pinned).unwrap_or(l.len());
                        l.insert(at, NoteSummary {
                            id: note.id,
                            title: note.title,
                            snippet: String::new(),
                            pinned: false,
                            updated_ms: note.updated_ms,
                        });
                    });
                    navigate(&format!("/notes/{}", note.id), Default::default());
                }
                Err(e) => {
                    let _ = window().alert_with_message(&format!("Couldn't create a note: {}", error_text(e)));
                }
            }
            creating.set(false);
        });
    };

    view! {
        <Title text="Notes · Workspace" />
        <section class="pane notes-pane">
            <div class="toolbar">
                <button class="aero-btn" disabled=move || creating.get() on:click=new_note.clone()>
                    <span class="btn-icon icon-new" aria-hidden="true"></span>
                    "New note"
                </button>
                <span class="spacer"></span>
                <span class="toolbar-note">"Saved automatically"</span>
            </div>

            <div class="notes-layout" class:has-selection=move || selected.get().is_some()>
                // ---------------- List ----------------
                <aside class="notes-list" aria-label="Notes">
                    <div class="notes-search">
                        <input
                            type="search"
                            placeholder="Search notes"
                            aria-label="Search notes"
                            prop:value=move || query.get()
                            on:input=on_search
                        />
                    </div>
                    <Suspense fallback=|| view! { <p class="notes-empty">"Loading…"</p> }>
                        {move || Suspend::new(async move {
                            match list.await {
                                Ok(items) => ctx.notes.set(items),
                                Err(e) => return view! { <p class="notes-empty">{error_text(e)}</p> }.into_any(),
                            }
                            view! {
                                <Show
                                    when=move || ctx.notes.with(|n| !n.is_empty())
                                    fallback=move || view! {
                                        <p class="notes-empty">
                                            {move || if search.with(|s| s.trim().is_empty()) {
                                                "No notes yet."
                                            } else {
                                                "No notes match your search."
                                            }}
                                        </p>
                                    }
                                >
                                    <ul class="notes-items">
                                        <For
                                            each=move || ctx.notes.get()
                                            key=|n| (n.id, n.title.clone(), n.snippet.clone(), n.pinned, n.updated_ms)
                                            children=move |n| view! { <NoteListItem note=n selected /> }
                                        />
                                    </ul>
                                </Show>
                            }
                            .into_any()
                        })}
                    </Suspense>
                </aside>

                // ---------------- Editor ----------------
                <div class="notes-editor-area">
                    {move || match selected.get() {
                        Some(id) => view! { <NoteEditor id /> }.into_any(),
                        None => view! {
                            <div class="notes-placeholder">
                                <div class="notes-hero" aria-hidden="true"></div>
                                <h2>"Your notes"</h2>
                                <p>"Pick a note on the left, or create a new one."</p>
                                <button class="aero-btn" on:click=new_note.clone()>"New note"</button>
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
fn NoteListItem(note: NoteSummary, selected: Memo<Option<i64>>) -> impl IntoView {
    let ctx = expect_context::<NotesCtx>();
    let id = note.id;
    let at = note.updated_ms;
    view! {
        <li>
            <a
                class="note-item"
                class:active=move || selected.get() == Some(id)
                aria-current=move || (selected.get() == Some(id)).then_some("page")
                href=format!("/notes/{id}")
            >
                <div class="note-item-title">
                    {note.pinned.then(|| view! { <span class="pin-dot" title="Pinned" aria-label="Pinned"></span> })}
                    <span class:untitled=note.title.trim().is_empty()>{display_title(&note.title)}</span>
                </div>
                <div class="note-item-snippet">
                    {if note.snippet.is_empty() { "No additional text".to_string() } else { note.snippet.clone() }}
                </div>
                <div class="note-item-time">{move || ctx.mounted.get().then(|| format_time(at as u64))}</div>
            </a>
        </li>
    }
}

#[derive(Clone, PartialEq)]
enum SaveStatus {
    Saved,
    Unsaved,
    Saving,
    Failed(String),
}

#[component]
fn NoteEditor(id: i64) -> impl IntoView {
    let note = Resource::new(move || id, get_note);
    view! {
        <Suspense fallback=|| view! { <div class="notes-placeholder"><p>"Opening…"</p></div> }>
            {move || Suspend::new(async move {
                match note.await {
                    Ok(Some(n)) => view! { <Editor note=n /> }.into_any(),
                    Ok(None) => view! {
                        <div class="notes-placeholder">
                            <h2>"Note not found"</h2>
                            <p>"It may have been deleted."</p>
                            <a href="/notes">"Back to notes"</a>
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
fn Editor(note: Note) -> impl IntoView {
    let ctx = expect_context::<NotesCtx>();
    let navigate = use_navigate();
    let id = note.id;

    // Arc signals: they must outlive this component for the final save.
    let title = ArcRwSignal::new(note.title);
    let body = ArcRwSignal::new(note.body);
    let pinned = RwSignal::new(note.pinned);
    let status = RwSignal::new(SaveStatus::Saved);
    let preview = RwSignal::new(false);
    let title_ref: NodeRef<html::Input> = NodeRef::new();

    // `edits` counts changes; `saved` is the count the server has. A new save
    // only starts when none is in flight, so an older save can never land
    // after a newer one.
    let edits = ArcStoredValue::new(0u64);
    let saved = ArcStoredValue::new(0u64);
    let in_flight = ArcStoredValue::new(false);

    let save = {
        let (title, body, edits, saved, in_flight) = (title.clone(), body.clone(), edits.clone(), saved.clone(), in_flight.clone());
        // Recursive through a shared slot so a finished save can start the next one.
        let slot: ArcStoredValue<Option<std::sync::Arc<dyn Fn() + Send + Sync>>> = ArcStoredValue::new(None);
        let run = {
            let slot = slot.clone();
            std::sync::Arc::new(move || {
                let target = edits.get_value();
                if target == saved.get_value() || in_flight.get_value() {
                    return;
                }
                in_flight.set_value(true);
                status.try_set(SaveStatus::Saving);
                let (t, b) = (title.get_untracked(), body.get_untracked());
                let (saved, in_flight, edits, slot) = (saved.clone(), in_flight.clone(), edits.clone(), slot.clone());
                spawn_local(async move {
                    let result = save_note(id, t.clone(), b.clone()).await;
                    in_flight.set_value(false);
                    match result {
                        Ok(updated_ms) => {
                            saved.set_value(target);
                            ctx.update(id, |n| {
                                n.title = t;
                                n.snippet = snippet(&b);
                                n.updated_ms = updated_ms;
                            });
                            if edits.get_value() > target {
                                if let Some(next) = slot.get_value() {
                                    next(); // more typing happened meanwhile
                                }
                            } else {
                                status.try_set(SaveStatus::Saved);
                            }
                        }
                        Err(e) => {
                            status.try_set(SaveStatus::Failed(error_text(e)));
                        }
                    }
                });
            }) as std::sync::Arc<dyn Fn() + Send + Sync>
        };
        slot.set_value(Some(run.clone()));
        run
    };

    let schedule = {
        let (edits, save) = (edits.clone(), save.clone());
        move || {
            edits.update_value(|e| *e += 1);
            status.set(SaveStatus::Unsaved);
            let this = edits.get_value();
            let (edits, save) = (edits.clone(), save.clone());
            set_timeout(
                move || {
                    if edits.get_value() == this {
                        save();
                    }
                },
                AUTOSAVE_DELAY,
            );
        }
    };

    // Save anything pending when switching notes or leaving the tab.
    {
        let save = save.clone();
        on_cleanup(move || save());
    }
    // Warn before closing the browser tab with unsaved changes.
    let unload = {
        let (edits, saved) = (edits.clone(), saved.clone());
        window_event_listener(leptos::ev::beforeunload, move |e| {
            if edits.get_value() != saved.get_value() {
                e.prevent_default();
            }
        })
    };
    on_cleanup(move || unload.remove());

    // New, empty notes start with the cursor in the title.
    if note_is_new(&title, &body) {
        request_animation_frame(move || {
            if let Some(el) = title_ref.get_untracked() {
                let _ = el.focus();
            }
        });
    }

    let toggle_pin = move |_| {
        let next = !pinned.get_untracked();
        pinned.set(next);
        ctx.update(id, |n| n.pinned = next);
        spawn_local(async move {
            if let Err(e) = pin_note(id, next).await {
                pinned.try_set(!next);
                ctx.update(id, |n| n.pinned = !next);
                status.try_set(SaveStatus::Failed(error_text(e)));
            }
        });
    };

    let remove = {
        let (title, edits, saved) = (title.clone(), edits.clone(), saved.clone());
        move |_| {
            let name = display_title(&title.get_untracked());
            if !window().confirm_with_message(&format!("Delete “{name}”? This can't be undone.")).unwrap_or(false) {
                return;
            }
            saved.set_value(edits.get_value()); // nothing left to save
            let navigate = navigate.clone();
            spawn_local(async move {
                match delete_note(id).await {
                    Ok(()) => {
                        ctx.notes.try_update(|l| l.retain(|n| n.id != id));
                        navigate("/notes", Default::default());
                    }
                    Err(e) => {
                        status.try_set(SaveStatus::Failed(error_text(e)));
                    }
                }
            });
        }
    };

    // ⌘S / Ctrl+S saves right away.
    let on_keydown = {
        let save = save.clone();
        move |ev: leptos::ev::KeyboardEvent| {
            if (ev.meta_key() || ev.ctrl_key()) && ev.key().eq_ignore_ascii_case("s") {
                ev.prevent_default();
                save();
            }
        }
    };

    let (title_r, title_w, body_r, body_w) = (title.clone(), title.clone(), body.clone(), body.clone());
    let (schedule_t, schedule_b) = (schedule.clone(), schedule);
    let body_preview = body.clone();

    view! {
        <div class="note-editor" on:keydown=on_keydown>
            <div class="note-bar">
                <a class="aero-btn small back-link" href="/notes">"‹ Notes"</a>
                <div class="segmented" role="group" aria-label="View">
                    <button type="button" class:active=move || !preview.get() aria-pressed=move || (!preview.get()).to_string() on:click=move |_| preview.set(false)>"Edit"</button>
                    <button type="button" class:active=move || preview.get() aria-pressed=move || preview.get().to_string() on:click=move |_| preview.set(true)>"Preview"</button>
                </div>
                <span class="spacer"></span>
                <span class="save-status" class:failed=move || matches!(status.get(), SaveStatus::Failed(_)) aria-live="polite">
                    {move || match status.get() {
                        SaveStatus::Saved => "Saved".to_string(),
                        SaveStatus::Unsaved => "Editing…".to_string(),
                        SaveStatus::Saving => "Saving…".to_string(),
                        SaveStatus::Failed(e) => format!("Not saved: {e}"),
                    }}
                </span>
                <button
                    type="button"
                    class="aero-btn small"
                    class:pinned=move || pinned.get()
                    aria-pressed=move || pinned.get().to_string()
                    title=move || if pinned.get() { "Unpin" } else { "Pin to the top of the list" }
                    on:click=toggle_pin
                >
                    <span class="pin-dot" aria-hidden="true"></span>
                    {move || if pinned.get() { "Pinned" } else { "Pin" }}
                </button>
                <button type="button" class="aero-btn small danger" on:click=remove>"Delete"</button>
            </div>

            <input
                class="note-title"
                type="text"
                placeholder="Title"
                aria-label="Title"
                node_ref=title_ref
                prop:value=move || title_r.get()
                on:input=move |ev| {
                    title_w.set(event_target_value(&ev));
                    schedule_t();
                }
                on:keydown=move |ev| {
                    // Enter in the title moves to the body.
                    if ev.key() == "Enter" {
                        ev.prevent_default();
                        if let Some(next) = document().query_selector(".note-body").ok().flatten() {
                            use wasm_bindgen::JsCast;
                            if let Ok(el) = next.dyn_into::<web_sys::HtmlElement>() {
                                let _ = el.focus();
                            }
                        }
                    }
                }
            />

            <Show
                when=move || preview.get()
                fallback=move || {
                    let (body_r, body_w, schedule_b) = (body_r.clone(), body_w.clone(), schedule_b.clone());
                    view! {
                        <textarea
                            class="note-body"
                            placeholder="Start writing… Markdown works: # heading, **bold**, - lists, ```code```"
                            aria-label="Note"
                            prop:value=move || body_r.get()
                            on:input=move |ev| {
                                body_w.set(event_target_value(&ev));
                                schedule_b();
                            }
                        ></textarea>
                    }
                }
            >
                {
                    let body_preview = body_preview.clone();
                    view! {
                        <div class="note-preview md" inner_html=move || {
                            let text = body_preview.get();
                            if text.trim().is_empty() {
                                "<p class=\"muted\">Nothing to preview yet.</p>".to_string()
                            } else {
                                markdown::render(&text)
                            }
                        }></div>
                    }
                }
            </Show>
        </div>
    }
}

fn note_is_new(title: &ArcRwSignal<String>, body: &ArcRwSignal<String>) -> bool {
    title.with_untracked(|t| t.is_empty()) && body.with_untracked(|b| b.is_empty())
}

#[cfg(test)]
mod tests {
    use super::{relevance, search_terms, snippet};

    #[test]
    fn search_is_loose_and_ranked() {
        assert_eq!(search_terms("What did I write about my grocery list?"), ["grocery".chars().take(5).collect::<String>(), "list".into()]);
        assert!(search_terms("the notes about it").is_empty());
        let t = search_terms("grocery");
        assert!(relevance(&t, "Groceries", "") > relevance(&t, "Misc", "buy groceries"));
        assert_eq!(relevance(&t, "Misc", "buy GROCERIES"), 1);
        assert_eq!(relevance(&search_terms("CAFÉ"), "", "um café"), 1); // non-ASCII case
        assert_eq!(relevance(&t, "Ideas", "nothing here"), 0);
    }

    #[test]
    fn snippet_strips_markdown_and_shortens() {
        assert_eq!(snippet("# Title\n\n- **bold** item\n> quote `code`"), "Title bold item quote code");
        assert_eq!(snippet("```rust\nfn main() {}\n```\nafter"), "fn main() {} after");
        let long = "word ".repeat(60);
        let s = snippet(&long);
        assert!(s.ends_with('…') && s.chars().count() == 120, "{s}");
        assert_eq!(snippet("   \n  "), "");
    }
}
