//! The Calculator module: a keypad calculator plus a shared history of
//! calculations made here and by the chat model's `calculate` tool.

use crate::calc;
use crate::chat::error_text;
use leptos::html;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_meta::Title;
use serde::{Deserialize, Serialize};

// ======================================================================
// Types shared by server and browser
// ======================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalcSource {
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Calculation {
    pub id: u64,
    pub expression: String,
    pub result: String,
    pub source: CalcSource,
    /// For the assistant: the chat message that led to the calculation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Unix time in milliseconds.
    pub at_ms: u64,
}

// ======================================================================
// Server functions
// ======================================================================

/// History, newest first.
#[server]
pub async fn get_calculations() -> Result<Vec<Calculation>, ServerFnError> {
    Ok(crate::server::state().await?.calculations().await)
}

/// Evaluates on the server (the source of truth) and saves to history.
#[server]
pub async fn run_calculation(expression: String) -> Result<Calculation, ServerFnError> {
    let result = calc::evaluate_to_string(&expression).map_err(|e| ServerFnError::new(e.to_string()))?;
    let state = crate::server::state().await?;
    Ok(state.record_calculation(expression.trim().to_string(), result, CalcSource::User, None).await)
}

#[server]
pub async fn clear_calculations() -> Result<(), ServerFnError> {
    crate::server::state().await?.clear_calculations().await;
    Ok(())
}

// ======================================================================
// Page
// ======================================================================

#[derive(Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Mine,
    Assistant,
}

impl Filter {
    fn allows(self, source: CalcSource) -> bool {
        match self {
            Filter::All => true,
            Filter::Mine => source == CalcSource::User,
            Filter::Assistant => source == CalcSource::Assistant,
        }
    }
}

/// Keypad layout: (label, what it inserts or does, extra class).
const KEYS: [(&str, Key, &str); 25] = [
    ("C", Key::Clear, "key-fn"),
    ("⌫", Key::Backspace, "key-fn"),
    ("(", Key::Insert("("), "key-op"),
    (")", Key::Insert(")"), "key-op"),
    ("÷", Key::Insert("÷"), "key-op"),
    ("7", Key::Insert("7"), ""),
    ("8", Key::Insert("8"), ""),
    ("9", Key::Insert("9"), ""),
    ("√", Key::Insert("√("), "key-op"),
    ("×", Key::Insert("×"), "key-op"),
    ("4", Key::Insert("4"), ""),
    ("5", Key::Insert("5"), ""),
    ("6", Key::Insert("6"), ""),
    ("xʸ", Key::Insert("^"), "key-op"),
    ("−", Key::Insert("−"), "key-op"),
    ("1", Key::Insert("1"), ""),
    ("2", Key::Insert("2"), ""),
    ("3", Key::Insert("3"), ""),
    ("%", Key::Insert("%"), "key-op"),
    ("+", Key::Insert("+"), "key-op"),
    ("0", Key::Insert("0"), ""),
    (".", Key::Insert("."), ""),
    ("π", Key::Insert("π"), "key-op"),
    ("ans", Key::Ans, "key-op"),
    ("=", Key::Equals, "key-equals"),
];

#[derive(Clone, Copy)]
enum Key {
    Insert(&'static str),
    Clear,
    Backspace,
    Ans,
    Equals,
}

#[component]
pub fn CalculatorPage() -> impl IntoView {
    let history = Resource::new(|| (), |_| get_calculations());
    let entries: RwSignal<Vec<Calculation>> = RwSignal::new(Vec::new());
    let loaded = StoredValue::new(false);

    let expr = RwSignal::new(String::new());
    // The last evaluated expression, shown above the input ("13*50/60 =").
    let committed: RwSignal<Option<String>> = RwSignal::new(None);
    let error: RwSignal<Option<String>> = RwSignal::new(None);
    let busy = RwSignal::new(false);
    let filter = RwSignal::new(Filter::All);
    let input: NodeRef<html::Input> = NodeRef::new();

    // Times are formatted in the browser's time zone, so only after hydration.
    let mounted = RwSignal::new(false);
    Effect::new(move |_| mounted.set(true));

    // Live preview while typing (evaluated locally; nothing is saved).
    let preview = Memo::new(move |_| {
        let e = expr.get();
        if e.trim().is_empty() || committed.with(|c| c.is_some()) {
            return None;
        }
        calc::evaluate_to_string(&e).ok()
    });

    let edit = move |new_value: String, cursor_utf16: u32| {
        expr.set(new_value);
        committed.set(None);
        error.set(None);
        request_animation_frame(move || {
            if let Some(el) = input.get_untracked() {
                let _ = el.focus();
                let _ = el.set_selection_range(cursor_utf16, cursor_utf16);
            }
        });
    };

    let insert = move |text: &str| {
        let value = expr.get_untracked();
        // After "=", typing a digit starts fresh; an operator continues from the result.
        let value = if committed.get_untracked().is_some() && text.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '.' || c == '(' || c == 'π' || c == '√') {
            String::new()
        } else {
            value
        };
        let (start, end) = selection(input, &value);
        let mut next = String::with_capacity(value.len() + text.len());
        next.push_str(&value[..start]);
        next.push_str(text);
        next.push_str(&value[end..]);
        let cursor = utf16_len(&next[..start + text.len()]);
        edit(next, cursor);
    };

    let backspace = move || {
        let value = expr.get_untracked();
        let (start, end) = selection(input, &value);
        let (cut_start, cut_end) = if start != end {
            (start, end)
        } else if start > 0 {
            let prev = value[..start].char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
            (prev, start)
        } else {
            return;
        };
        let next = format!("{}{}", &value[..cut_start], &value[cut_end..]);
        let cursor = utf16_len(&next[..cut_start]);
        edit(next, cursor);
    };

    let clear = move || edit(String::new(), 0);

    let equals = move || {
        let expression = expr.get_untracked();
        if expression.trim().is_empty() || busy.get_untracked() || committed.get_untracked().is_some() {
            return;
        }
        // Show mistakes immediately without a round trip.
        if let Err(e) = calc::evaluate(&expression) {
            error.set(Some(e.to_string()));
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match run_calculation(expression.clone()).await {
                Ok(c) => {
                    committed.set(Some(c.expression.clone()));
                    expr.set(c.result.clone());
                    error.set(None);
                    entries.update(|list| list.insert(0, c));
                }
                Err(e) => error.set(Some(error_text(e))),
            }
            busy.set(false);
            if let Some(el) = input.get_untracked() {
                let _ = el.focus();
            }
        });
    };

    let ans = move || {
        let last = entries.with_untracked(|l| l.first().map(|c| c.result.clone()));
        if let Some(result) = last {
            insert(&result);
        }
    };

    let press = move |key: Key| match key {
        Key::Insert(text) => insert(text),
        Key::Clear => clear(),
        Key::Backspace => backspace(),
        Key::Ans => ans(),
        Key::Equals => equals(),
    };

    let pick = move |c: Calculation| {
        let cursor = utf16_len(&c.expression);
        edit(c.expression, cursor);
    };

    let clear_history = move |_| {
        let confirmed = window().confirm_with_message("Clear the whole calculation history?").unwrap_or(false);
        if confirmed {
            entries.set(Vec::new());
            spawn_local(async {
                let _ = clear_calculations().await;
            });
        }
    };

    view! {
        <Title text="Calculator · Terrarium" />
        <section class="pane calc-pane">
            <div class="toolbar">
                <span class="toolbar-title">"Calculator"</span>
                <span class="spacer"></span>
                <span class="toolbar-note">"Calculations from the chat show up here too"</span>
            </div>

            <div class="calc-scroll">
                <div class="calc-layout">
                    // ---------------- The calculator ----------------
                    <div class="calculator">
                        <div class="calc-display" class:has-error=move || error.with(Option::is_some)>
                            <div class="display-top">
                                {move || committed.get().map(|c| format!("{c} ="))}
                            </div>
                            <input
                                class="display-input"
                                type="text"
                                inputmode="decimal"
                                spellcheck="false"
                                autocomplete="off"
                                aria-label="Expression"
                                placeholder="0"
                                node_ref=input
                                prop:value=move || expr.get()
                                on:input=move |ev| {
                                    expr.set(event_target_value(&ev));
                                    committed.set(None);
                                    error.set(None);
                                }
                                on:keydown=move |ev| match ev.key().as_str() {
                                    "Enter" | "=" => {
                                        ev.prevent_default();
                                        equals();
                                    }
                                    "Escape" => {
                                        ev.prevent_default();
                                        clear();
                                    }
                                    _ => {}
                                }
                            />
                            <div class="display-bottom" aria-live="polite">
                                {move || match (error.get(), preview.get()) {
                                    (Some(e), _) => view! { <span class="display-error">{e}</span> }.into_any(),
                                    (None, Some(p)) => view! { <span class="display-preview">"= " {p}</span> }.into_any(),
                                    (None, None) => ().into_any(),
                                }}
                            </div>
                        </div>

                        <div class="keypad">
                            {KEYS
                                .iter()
                                .map(|&(label, key, class)| {
                                    view! {
                                        <button
                                            type="button"
                                            class=format!("key {class}")
                                            disabled=move || matches!(key, Key::Equals) && busy.get()
                                            on:click=move |_| press(key)
                                        >
                                            {label}
                                        </button>
                                    }
                                })
                                .collect_view()}
                        </div>
                        <p class="calc-help">
                            "Type too: " <code>"sqrt round(x, 2) min max ln log sin cos abs ^ % ! mod gcd lcm isprime prime(k) if(c, a, b)"</code>
                            <br />
                            "Series: " <code>"sum(prime(k), k, 1, 100)"</code>
                        </p>
                    </div>

                    // ---------------- History ----------------
                    <div class="history">
                        <div class="history-head">
                            <h2>"History"</h2>
                            <div class="segmented" role="group" aria-label="Show">
                                {[(Filter::All, "All"), (Filter::Mine, "Mine"), (Filter::Assistant, "Assistant")]
                                    .into_iter()
                                    .map(|(f, label)| view! {
                                        <button
                                            type="button"
                                            class:active=move || filter.get() == f
                                            aria-pressed=move || (filter.get() == f).to_string()
                                            on:click=move |_| filter.set(f)
                                        >
                                            {label}
                                        </button>
                                    })
                                    .collect_view()}
                            </div>
                            <button
                                type="button"
                                class="aero-btn small"
                                disabled=move || entries.with(Vec::is_empty)
                                on:click=clear_history
                            >
                                "Clear"
                            </button>
                        </div>

                        <Suspense fallback=|| view! { <p class="history-empty">"Loading…"</p> }>
                            {move || Suspend::new(async move {
                                if let Ok(list) = history.await {
                                    if !loaded.get_value() {
                                        loaded.set_value(true);
                                        entries.set(list);
                                    }
                                }
                                let visible = move || {
                                    let f = filter.get();
                                    entries.get().into_iter().filter(|c| f.allows(c.source)).collect::<Vec<_>>()
                                };
                                view! {
                                    <Show
                                        when=move || !visible().is_empty()
                                        fallback=move || view! {
                                            <p class="history-empty">
                                                {move || match filter.get() {
                                                    Filter::Assistant => "The assistant hasn't calculated anything yet. Ask the chat a question with numbers in it.",
                                                    Filter::Mine => "Your calculations will appear here.",
                                                    Filter::All => "No calculations yet. Use the calculator, or ask the chat something with numbers.",
                                                }}
                                            </p>
                                        }
                                    >
                                        <ol class="history-list">
                                            <For
                                                each=visible
                                                key=|c| c.id
                                                children=move |c| view! { <HistoryItem calc=c mounted on_pick=pick /> }
                                            />
                                        </ol>
                                    </Show>
                                }
                            })}
                        </Suspense>
                    </div>
                </div>
            </div>
        </section>
    }
}

#[component]
fn HistoryItem(calc: Calculation, mounted: RwSignal<bool>, on_pick: impl Fn(Calculation) + Copy + Send + Sync + 'static) -> impl IntoView {
    let at = calc.at_ms;
    let source = calc.source;
    let picked = calc.clone();
    view! {
        <li class="history-item">
            <button type="button" class="history-btn" title="Load into the calculator" on:click=move |_| on_pick(picked.clone())>
                <div class="history-expr">{calc.expression.clone()}</div>
                <div class="history-result">"= " {calc.result.clone()}</div>
                <div class="history-meta">
                    {match source {
                        CalcSource::User => view! { <span class="badge badge-user"><span class="badge-orb"></span>"You"</span> }.into_any(),
                        CalcSource::Assistant => view! { <span class="badge badge-assistant"><span class="badge-orb"></span>"Assistant"</span> }.into_any(),
                    }}
                    <span class="history-time">{move || mounted.get().then(|| format_time(at))}</span>
                </div>
                {calc.context.map(|ctx| view! { <div class="history-context">"In chat: “" {ctx} "”"</div> })}
            </button>
        </li>
    }
}

// ======================================================================
// Helpers
// ======================================================================

/// The input's selection as byte offsets into `value` (end of text if unknown).
fn selection(input: NodeRef<html::Input>, value: &str) -> (usize, usize) {
    let end_of_text = value.len();
    let Some(el) = input.get_untracked() else { return (end_of_text, end_of_text) };
    let start = el.selection_start().ok().flatten();
    let end = el.selection_end().ok().flatten();
    match (start, end) {
        (Some(s), Some(e)) => (utf16_to_byte(value, s), utf16_to_byte(value, e)),
        _ => (end_of_text, end_of_text),
    }
}

/// Browsers count text positions in UTF-16 units; Rust strings use bytes.
fn utf16_to_byte(s: &str, utf16_index: u32) -> usize {
    let mut units = 0u32;
    for (byte, c) in s.char_indices() {
        if units >= utf16_index {
            return byte;
        }
        units += c.len_utf16() as u32;
    }
    s.len()
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// "14:05" for today, "3 Nov, 14:05" otherwise, in the viewer's locale.
/// Browser only (call after hydration).
pub(crate) fn format_time(at_ms: u64) -> String {
    let date = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(at_ms as f64));
    let now = js_sys::Date::new_0();
    let locale = window().navigator().language().unwrap_or_else(|| "en-US".into());

    let options = js_sys::Object::new();
    let set = |k: &str, v: &str| {
        let _ = js_sys::Reflect::set(&options, &k.into(), &v.into());
    };
    set("hour", "2-digit");
    set("minute", "2-digit");
    let time: String = date.to_locale_time_string_with_options(&locale, &options).into();

    if date.to_date_string() == now.to_date_string() {
        time
    } else {
        let day_options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&day_options, &"day".into(), &"numeric".into());
        let _ = js_sys::Reflect::set(&day_options, &"month".into(), &"short".into());
        let day: String = date.to_locale_date_string(&locale, &day_options).into();
        format!("{day}, {time}")
    }
}
