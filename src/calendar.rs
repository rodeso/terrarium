//! The Calendar module: a month view with events stored in the workspace
//! database. Dates and times are plain local values ("2026-11-14", "15:30").

use crate::chat::error_text;
use crate::dates::{Date, WEEKDAYS, parse_time};
use leptos::prelude::*;
use leptos::server_fn::codec::Json;
use leptos::task::spawn_local;
use leptos_meta::Title;
use leptos_router::hooks::use_params_map;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Event chips shown inside a month cell before "+N more".
const CHIPS_PER_DAY: usize = 3;

// ======================================================================
// Types shared by server and browser
// ======================================================================

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub title: String,
    /// "YYYY-MM-DD"
    pub date: String,
    /// "HH:MM"; None = all day.
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub location: String,
    pub notes: String,
}

impl Event {
    /// "All day", "15:30" or "15:30–16:30".
    pub fn when(&self) -> String {
        match (&self.start_time, &self.end_time) {
            (None, _) => "All day".into(),
            (Some(s), None) => s.clone(),
            (Some(s), Some(e)) => format!("{s}–{e}"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EventInput {
    /// None creates a new event.
    pub id: Option<i64>,
    pub title: String,
    pub date: String,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
    pub location: String,
    pub notes: String,
}

impl EventInput {
    fn from_event(e: &Event) -> Self {
        Self {
            id: Some(e.id),
            title: e.title.clone(),
            date: e.date.clone(),
            start_time: e.start_time.clone(),
            end_time: e.end_time.clone(),
            location: e.location.clone(),
            notes: e.notes.clone(),
        }
    }
}

/// Checks and normalizes an event. Used by the form (instant feedback), the
/// server function, and the model's `add_event` tool.
pub fn validate(input: EventInput) -> Result<EventInput, String> {
    let title = input.title.trim().to_string();
    if title.is_empty() {
        return Err("Give the event a title".into());
    }
    if title.chars().count() > 200 {
        return Err("The title is too long (200 characters at most)".into());
    }
    let date = Date::parse(&input.date).ok_or_else(|| format!("“{}” isn't a valid date (use YYYY-MM-DD)", input.date))?;
    let time = |t: Option<String>, what: &str| -> Result<Option<String>, String> {
        match t.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
            None => Ok(None),
            Some(t) => parse_time(t).map(Some).ok_or_else(|| format!("“{t}” isn't a valid {what} time (use HH:MM)")),
        }
    };
    let start_time = time(input.start_time, "start")?;
    let end_time = time(input.end_time, "end")?;
    if start_time.is_none() && end_time.is_some() {
        return Err("Add a start time, or remove the end time for an all-day event".into());
    }
    if let (Some(s), Some(e)) = (&start_time, &end_time) {
        if e <= s {
            return Err("The end time must be after the start time".into());
        }
    }
    Ok(EventInput {
        id: input.id,
        title,
        date: date.to_string(),
        start_time,
        end_time,
        location: input.location.trim().to_string(),
        notes: input.notes.trim().to_string(),
    })
}

/// Today's date where the code is running. The app runs locally, so the
/// server and the browser share a clock and a time zone.
pub fn today() -> Date {
    #[cfg(feature = "ssr")]
    {
        use chrono::Datelike;
        let now = chrono::Local::now();
        Date::new(now.year(), now.month(), now.day()).expect("valid date")
    }
    #[cfg(not(feature = "ssr"))]
    {
        let now = js_sys::Date::new_0();
        Date::new(now.get_full_year() as i32, now.get_month() + 1, now.get_date()).expect("valid date")
    }
}

// ======================================================================
// Server functions
// ======================================================================

#[server]
pub async fn list_events(from: String, to: String) -> Result<Vec<Event>, ServerFnError> {
    let (Some(from), Some(to)) = (Date::parse(&from), Date::parse(&to)) else {
        return Err(ServerFnError::new("Dates must look like YYYY-MM-DD"));
    };
    crate::server::state().await?.db.events_between(from.to_string(), to.to_string()).await.map_err(ServerFnError::new)
}

#[server(input = Json)]
pub async fn save_event(event: EventInput) -> Result<Event, ServerFnError> {
    let event = validate(event).map_err(ServerFnError::new)?;
    crate::server::state()
        .await?
        .db
        .save_event(event)
        .await
        .map_err(ServerFnError::new)?
        .ok_or_else(|| ServerFnError::new("This event was deleted"))
}

#[server]
pub async fn delete_event(id: i64) -> Result<(), ServerFnError> {
    crate::server::state().await?.db.delete_event(id).await.map_err(ServerFnError::new)
}

// ======================================================================
// Page
// ======================================================================

/// What the day panel is showing.
#[derive(Clone, PartialEq)]
enum Panel {
    List,
    Edit(EventInput),
}

#[component]
pub fn CalendarPage() -> impl IntoView {
    let params = use_params_map();
    let selected = Memo::new(move |_| params.with(|p| p.get("date").and_then(|d| Date::parse(&d))).unwrap_or_else(today));
    let today = today();

    let reload = RwSignal::new(0u32);
    // Events for the whole visible grid (6 weeks), refetched when the month changes.
    let range = Memo::new(move |_| {
        let grid = selected.get().month_grid();
        (grid[0].to_string(), grid[41].to_string())
    });
    let events = Resource::new(move || (range.get(), reload.get()), |((from, to), _)| list_events(from, to));
    let by_day: RwSignal<HashMap<String, Vec<Event>>> = RwSignal::new(HashMap::new());

    let panel = RwSignal::new(Panel::List);
    // Switching days closes any open form.
    Effect::new(move |prev: Option<Date>| {
        let d = selected.get();
        if prev.is_some_and(|p| p != d) {
            panel.set(Panel::List);
        }
        d
    });

    let new_event = move || {
        panel.set(Panel::Edit(EventInput { date: selected.get_untracked().to_string(), ..Default::default() }));
    };

    view! {
        <Title text="Calendar · Workspace" />
        <section class="pane calendar-pane">
            <div class="toolbar">
                <a class="aero-btn small" href=move || format!("/calendar/{}", selected.get().add_months(-1)) title="Previous month" aria-label="Previous month">"‹"</a>
                <a class="aero-btn small" href=format!("/calendar/{today}")>"Today"</a>
                <a class="aero-btn small" href=move || format!("/calendar/{}", selected.get().add_months(1)) title="Next month" aria-label="Next month">"›"</a>
                <h2 class="month-title">{move || selected.get().month_title()}</h2>
                <span class="spacer"></span>
                <button class="aero-btn" on:click=move |_| new_event()>
                    <span class="btn-icon icon-new" aria-hidden="true"></span>
                    "New event"
                </button>
            </div>

            <div class="calendar-layout">
                <div class="month">
                    <div class="weekday-row" aria-hidden="true">
                        {WEEKDAYS.iter().map(|w| view! { <div>{&w[..3]}</div> }).collect_view()}
                    </div>
                    <Suspense fallback=|| ()>
                        {move || Suspend::new(async move {
                            let list = match events.await {
                                Ok(l) => l,
                                Err(e) => return view! { <p class="notes-empty">{error_text(e)}</p> }.into_any(),
                            };
                            let mut map: HashMap<String, Vec<Event>> = HashMap::new();
                            for e in list {
                                map.entry(e.date.clone()).or_default().push(e);
                            }
                            by_day.set(map);
                            view! {
                                <div class="month-grid" role="grid">
                                    {move || selected.get().month_grid().into_iter().map(|d| {
                                        let key = d.to_string();
                                        let day_events = by_day.with(|m| m.get(&key).cloned().unwrap_or_default());
                                        let count = day_events.len();
                                        let more = count.saturating_sub(CHIPS_PER_DAY);
                                        let in_month = d.month == selected.get_untracked().month;
                                        view! {
                                            <a
                                                class="day"
                                                class:other-month=!in_month
                                                class:today={d == today}
                                                class:selected=move || selected.get() == d
                                                class:weekend={d.weekday() >= 5}
                                                href=format!("/calendar/{key}")
                                                aria-label=format!("{}, {count} event(s)", d.long())
                                                role="gridcell"
                                            >
                                                <span class="day-num">{d.day}</span>
                                                {day_events.into_iter().take(CHIPS_PER_DAY).map(|e| view! {
                                                    <span class="event-chip" class:all-day=e.start_time.is_none()>
                                                        {e.start_time.clone().map(|t| view! { <b>{t}</b>" " })}
                                                        {e.title.clone()}
                                                    </span>
                                                }).collect_view()}
                                                {(more > 0).then(|| view! { <span class="more">{format!("+{more} more")}</span> })}
                                            </a>
                                        }
                                    }).collect_view()}
                                </div>
                            }
                            .into_any()
                        })}
                    </Suspense>
                </div>

                <aside class="day-panel">
                    <div class="day-head">
                        <h3>{move || selected.get().long()}</h3>
                        {move || (selected.get() == today).then(|| view! { <span class="today-badge">"Today"</span> })}
                    </div>
                    {move || match panel.get() {
                        Panel::Edit(input) => view! {
                            <EventForm
                                input
                                on_done=move |changed| {
                                    panel.set(Panel::List);
                                    if changed {
                                        reload.update(|n| *n += 1);
                                    }
                                }
                            />
                        }
                        .into_any(),
                        Panel::List => {
                            let key = selected.get().to_string();
                            let list = by_day.with(|m| m.get(&key).cloned().unwrap_or_default());
                            view! {
                                <Show
                                    when={
                                        let empty = list.is_empty();
                                        move || !empty
                                    }
                                    fallback=|| view! { <p class="day-empty">"Nothing planned."</p> }
                                >
                                    <ul class="day-events">
                                        {list.clone().into_iter().map(|e| {
                                            let input = EventInput::from_event(&e);
                                            view! {
                                                <li>
                                                    <button type="button" class="day-event" on:click=move |_| panel.set(Panel::Edit(input.clone()))>
                                                        <span class="when">{e.when()}</span>
                                                        <span class="what">
                                                            <span class="title">{e.title.clone()}</span>
                                                            {(!e.location.is_empty()).then(|| view! { <span class="where">{e.location.clone()}</span> })}
                                                            {(!e.notes.is_empty()).then(|| view! { <span class="notes">{e.notes.clone()}</span> })}
                                                        </span>
                                                    </button>
                                                </li>
                                            }
                                        }).collect_view()}
                                    </ul>
                                </Show>
                                <button class="aero-btn add-event" on:click=move |_| new_event()>"+ Add event"</button>
                            }
                            .into_any()
                        }
                    }}
                </aside>
            </div>
        </section>
    }
}

/// Create or edit an event. `on_done(true)` after a save or delete.
#[component]
fn EventForm(input: EventInput, on_done: impl Fn(bool) + Copy + Send + Sync + 'static) -> impl IntoView {
    let id = input.id;
    let title = RwSignal::new(input.title);
    let date = RwSignal::new(input.date);
    let all_day = RwSignal::new(input.start_time.is_none() && id.is_some());
    let start = RwSignal::new(input.start_time.unwrap_or_else(|| "09:00".into()));
    let end = RwSignal::new(input.end_time.unwrap_or_default());
    let location = RwSignal::new(input.location);
    let notes = RwSignal::new(input.notes);
    let error: RwSignal<Option<String>> = RwSignal::new(None);
    let busy = RwSignal::new(false);

    let save = move || {
        if busy.get_untracked() {
            return;
        }
        let candidate = EventInput {
            id,
            title: title.get_untracked(),
            date: date.get_untracked(),
            start_time: (!all_day.get_untracked()).then(|| start.get_untracked()),
            end_time: (!all_day.get_untracked()).then(|| end.get_untracked()).filter(|e| !e.trim().is_empty()),
            location: location.get_untracked(),
            notes: notes.get_untracked(),
        };
        let checked = match validate(candidate) {
            Ok(c) => c,
            Err(e) => return error.set(Some(e)),
        };
        busy.set(true);
        spawn_local(async move {
            match save_event(checked).await {
                Ok(_) => on_done(true),
                Err(e) => {
                    error.try_set(Some(error_text(e)));
                    busy.try_set(false);
                }
            }
        });
    };

    let remove = move |_| {
        let Some(id) = id else { return };
        let name = title.get_untracked();
        if !window().confirm_with_message(&format!("Delete “{name}”?")).unwrap_or(false) {
            return;
        }
        busy.set(true);
        spawn_local(async move {
            match delete_event(id).await {
                Ok(()) => on_done(true),
                Err(e) => {
                    error.try_set(Some(error_text(e)));
                    busy.try_set(false);
                }
            }
        });
    };

    let field = |sig: RwSignal<String>| move |ev| sig.set(event_target_value(&ev));

    view! {
        <form class="event-form" on:submit=move |ev| { ev.prevent_default(); save(); }>
            <h4>{if id.is_some() { "Edit event" } else { "New event" }}</h4>
            <label>
                "Title"
                <input type="text" required autofocus prop:value=move || title.get() on:input=field(title) placeholder="e.g. Dentist" />
            </label>
            <label>
                "Date"
                <input type="date" required prop:value=move || date.get() on:input=field(date) />
            </label>
            <label class="check">
                <input type="checkbox" prop:checked=move || all_day.get() on:change=move |ev| all_day.set(event_target_checked(&ev)) />
                "All day"
            </label>
            <Show when=move || !all_day.get()>
                <div class="time-row">
                    <label>"Starts" <input type="time" prop:value=move || start.get() on:input=field(start) /></label>
                    <label>"Ends" <input type="time" prop:value=move || end.get() on:input=field(end) /></label>
                </div>
            </Show>
            <label>
                "Location"
                <input type="text" prop:value=move || location.get() on:input=field(location) placeholder="Optional" />
            </label>
            <label>
                "Notes"
                <textarea rows="3" prop:value=move || notes.get() on:input=field(notes) placeholder="Optional"></textarea>
            </label>
            {move || error.get().map(|e| view! { <p class="feed-error" role="alert">{e}</p> })}
            <div class="form-buttons">
                {id.is_some().then(|| view! {
                    <button type="button" class="aero-btn small danger" disabled=move || busy.get() on:click=remove>"Delete"</button>
                })}
                <span class="spacer"></span>
                <button type="button" class="aero-btn small" on:click=move |_| on_done(false)>"Cancel"</button>
                <button type="submit" class="aero-btn" disabled=move || busy.get()>
                    {move || if busy.get() { "Saving…" } else { "Save" }}
                </button>
            </div>
        </form>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(date: &str, start: Option<&str>, end: Option<&str>) -> EventInput {
        EventInput {
            title: "  Dentist ".into(),
            date: date.into(),
            start_time: start.map(Into::into),
            end_time: end.map(Into::into),
            ..Default::default()
        }
    }

    #[test]
    fn validation_normalizes_and_rejects() {
        let ok = validate(input("2026-11-14", Some("15h30"), Some("16:30"))).unwrap();
        assert_eq!((ok.title.as_str(), ok.start_time.as_deref()), ("Dentist", Some("15:30")));
        assert!(validate(input("2026-11-14", None, None)).unwrap().start_time.is_none()); // all day
        assert!(validate(input("2026-11-14", Some(""), None)).unwrap().start_time.is_none());
        assert!(validate(input("tomorrow", None, None)).is_err());
        assert!(validate(input("2026-11-14", Some("16:00"), Some("15:00"))).is_err());
        assert!(validate(input("2026-11-14", None, Some("15:00"))).is_err());
        assert!(validate(EventInput { title: " ".into(), date: "2026-11-14".into(), ..Default::default() }).is_err());
    }
}
