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

/// Themes in the title bar picker: (id, name, description). The id is the
/// `data-theme` value on `<html>` that style/main.css keys on; "aero" is the
/// default and has no overrides.
const THEMES: [(&str, &str, &str); 5] = [
    ("aero", "Frutiger Aero", "Sky, grass and glossy glass"),
    ("eco", "Frutiger Eco", "Leaves, dew drops and daylight"),
    ("dark-aero", "Dark Aero", "Black glass and aurora glow"),
    ("technozen", "Technozen", "Silver calm and still water"),
    ("dorfic", "DORFic", "White steel with orange accents"),
];
const THEME_KEY: &str = "theme";

/// Runs before the first paint so a saved theme never flashes the default.
/// `<html>` is outside the hydrated body, so setting it here is safe.
const THEME_BOOT: &str =
    "try{var t=localStorage.getItem('theme');if(t)document.documentElement.setAttribute('data-theme',t)}catch(e){}";

pub fn shell(options: LeptosOptions) -> impl IntoView {
    view! {
        <!DOCTYPE html>
        <html lang="en">
            <head>
                <meta charset="utf-8" />
                <meta name="viewport" content="width=device-width, initial-scale=1" />
                <link rel="icon" href="/favicon.svg" type="image/svg+xml" />
                <script inner_html=THEME_BOOT></script>
                <AutoReload options=options.clone() />
                <HydrationScripts options />
                <MetaTags />
            </head>
            <body>
                <App />
            </body>
        </html>
    }
}

/// Sky backdrop, glass window, sidebar, and the active module.
#[component]
pub fn App() -> impl IntoView {
    provide_meta_context();
    // App-level so a streaming reply survives switching tabs.
    provide_context(Chat::new());

    view! {
        <Stylesheet id="leptos" href="/pkg/terrarium.css" />
        <Title text="Terrarium" />

        <div class="backdrop" aria-hidden="true">
            {(0..9).map(|i| view! { <span class=format!("orb orb-{i}")></span> }).collect_view()}
        </div>

        <Router>
            <div class="window">
                <div class="titlebar">
                    <span class="app-icon" aria-hidden="true"></span>
                    <span class="title">"Terrarium"</span>
                    <ThemePicker />
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
                <div class="app-version">{concat!("Terrarium v", env!("CARGO_PKG_VERSION"))}</div>
            </div>
        </nav>
    }
}

/// Title bar menu for switching themes. The choice is saved in this
/// browser's localStorage and applied to `<html data-theme>` at once.
#[component]
fn ThemePicker() -> impl IntoView {
    let current = RwSignal::new(THEMES[0].0.to_string());
    let open = RwSignal::new(false);
    // Read what the boot script applied (browser only).
    Effect::new(move |_| {
        if let Some(t) = document().document_element().and_then(|e| e.get_attribute("data-theme")) {
            if THEMES.iter().any(|&(id, ..)| id == t) {
                current.set(t);
            }
        }
    });
    let choose = move |id: &'static str| {
        current.set(id.to_string());
        open.set(false);
        if let Some(root) = document().document_element() {
            let _ = root.set_attribute("data-theme", id);
        }
        if let Ok(Some(storage)) = window().local_storage() {
            let _ = storage.set_item(THEME_KEY, id);
        }
    };
    let name = move || {
        current.with(|c| THEMES.iter().find(|&&(id, ..)| id == c).map_or(THEMES[0].1, |&(_, n, _)| n))
    };

    view! {
        <div
            class="theme-picker"
            on:keydown=move |ev| {
                if ev.key() == "Escape" {
                    open.set(false);
                }
            }
        >
            <button
                class="theme-btn"
                aria-haspopup="menu"
                aria-expanded=move || open.get().to_string()
                title=move || format!("Theme: {}", name())
                on:click=move |_| open.update(|o| *o = !*o)
            >
                <span class="theme-swatch" aria-hidden="true"></span>
                "Theme"
            </button>
            <Show when=move || open.get()>
                <div class="menu-scrim" on:click=move |_| open.set(false)></div>
                <div class="theme-menu" role="menu" aria-label="Theme">
                    <div class="theme-menu-head">"Choose a theme"</div>
                    {THEMES
                        .iter()
                        .map(|&(id, label, blurb)| {
                            let active = move || current.with(|c| c == id);
                            view! {
                                <button
                                    class="theme-option"
                                    class:active=active
                                    role="menuitemradio"
                                    aria-checked=move || active().to_string()
                                    on:click=move |_| choose(id)
                                >
                                    <span class=format!("theme-preview preview-{id}") aria-hidden="true">
                                        <i></i>
                                        <i></i>
                                        <i></i>
                                    </span>
                                    <span>
                                        <b>{label}</b>
                                        <small>{blurb}</small>
                                    </span>
                                    <span class="tick" aria-hidden="true">{move || active().then_some("✓")}</span>
                                </button>
                            }
                        })
                        .collect_view()}
                </div>
            </Show>
        </div>
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
