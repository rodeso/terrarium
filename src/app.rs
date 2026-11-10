//! The app shell: document, window chrome, sidebar navigation and routing.
//! Each module (Chat, Calculator, …) lives in its own file and route.

use crate::calculator::CalculatorPage;
use crate::calendar::CalendarPage;
use crate::chat::{Chat, ChatPage};
use crate::news::NewsPage;
use crate::notes::NotesPage;
use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_location;
use leptos_router::path;

/// Modules shown in the sidebar: (label, path, icon class).
/// To add one: add an entry here and a matching `<Route>` below.
const MODULES: [(&str, &str, &str); 5] = [
    ("Chat", "/", "icon-chat"),
    ("Calendar", "/calendar", "icon-calendar"),
    ("Notes", "/notes", "icon-notes"),
    ("Newsstand", "/news", "icon-news"),
    ("Calculator", "/calculator", "icon-calc"),
];

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
                <AutoReload options=options.clone() />
                <HydrationScripts options />
                <MetaTags />
            </head>
            <body>
                <Workspace />
            </body>
        </html>
    }
}

/// Sky backdrop, glass window, sidebar, and the active module.
#[component]
pub fn Workspace() -> impl IntoView {
    provide_meta_context();
    // App-level so a streaming reply survives switching tabs.
    provide_context(Chat::new());

    view! {
        <Stylesheet id="leptos" href="/pkg/ollama-web-app.css" />
        <Title text="Workspace" />

        <div class="backdrop" aria-hidden="true">
            {(0..9).map(|i| view! { <span class=format!("orb orb-{i}")></span> }).collect_view()}
        </div>

        <Router>
            <div class="window">
                <div class="titlebar">
                    <span class="app-icon" aria-hidden="true"></span>
                    <span class="title">"Workspace"</span>
                </div>
                <div class="window-body">
                    <Sidebar />
                    <Routes fallback=|| view! {
                        <section class="pane not-found">
                            <h2>"Page not found"</h2>
                            <a href="/">"Back to Chat"</a>
                        </section>
                    }>
                        <Route path=path!("/") view=ChatPage />
                        <Route path=path!("/notes") view=NotesPage />
                        <Route path=path!("/notes/:id") view=NotesPage />
                        <Route path=path!("/calendar") view=CalendarPage />
                        <Route path=path!("/calendar/:date") view=CalendarPage />
                        <Route path=path!("/news") view=NewsPage />
                        <Route path=path!("/news/:id") view=NewsPage />
                        <Route path=path!("/calculator") view=CalculatorPage />
                    </Routes>
                </div>
            </div>
        </Router>
    }
}

#[component]
fn Sidebar() -> impl IntoView {
    let location = use_location();
    view! {
        <nav class="sidebar" aria-label="Apps">
            <div class="nav-heading">"Apps"</div>
            {MODULES
                .iter()
                .map(|&(label, href, icon)| {
                    // "/notes" is also active on "/notes/12"; "/" only on "/".
                    let active = move || {
                        location.pathname.with(|p| {
                            p == href || (href != "/" && p.starts_with(href) && p[href.len()..].starts_with('/'))
                        })
                    };
                    view! {
                        <a
                            class="nav-item"
                            class:active=active
                            href=href
                            aria-current=move || active().then_some("page")
                        >
                            <span class=format!("nav-icon {icon}") aria-hidden="true"></span>
                            {label}
                        </a>
                    }
                })
                .collect_view()}
            <div class="sidebar-foot">
                <Clock />
                <div>"Running locally with Ollama"</div>
            </div>
        </nav>
    }
}

/// Time and date in the sidebar footer, like the Windows 7 taskbar clock.
/// Browser only: the server doesn't know the viewer's time zone, so nothing
/// is rendered until the page is running in the browser.
#[component]
fn Clock() -> impl IntoView {
    let now: RwSignal<Option<(String, String)>> = RwSignal::new(None);
    Effect::new(move |_| {
        let tick = move || {
            let next = clock_text();
            if now.get_untracked().as_ref() != Some(&next) {
                now.set(Some(next));
            }
        };
        tick();
        // Checked every second so the minute flips on time; it only
        // re-renders when the text changes.
        if let Ok(handle) = set_interval_with_handle(tick, std::time::Duration::from_secs(1)) {
            on_cleanup(move || handle.clear());
        }
    });
    view! {
        <div class="clock" title=move || now.get().map(|(t, d)| format!("{d}, {t}"))>
            {move || now.get().map(|(time, date)| view! {
                <div class="clock-time">{time}</div>
                <div class="clock-date">{date}</div>
            })}
        </div>
    }
}

/// ("14:05", "Tuesday, 10 November 2026") in the viewer's language and format.
fn clock_text() -> (String, String) {
    let date = js_sys::Date::new_0();
    let locale = window().navigator().language().unwrap_or_else(|| "en-US".into());
    let options = |pairs: &[(&str, &str)]| {
        let o = js_sys::Object::new();
        for (k, v) in pairs {
            let _ = js_sys::Reflect::set(&o, &(*k).into(), &(*v).into());
        }
        o
    };
    let time: String = date.to_locale_time_string_with_options(&locale, &options(&[("hour", "2-digit"), ("minute", "2-digit")])).into();
    let day: String = date
        .to_locale_date_string(&locale, &options(&[("weekday", "long"), ("day", "numeric"), ("month", "long"), ("year", "numeric")]))
        .into();
    (time, day)
}
