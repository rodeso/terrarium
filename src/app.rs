//! The app shell: document, window chrome, sidebar navigation and routing.
//! Each module (Chat, Calculator, …) lives in its own file and route.

use crate::calculator::CalculatorPage;
use crate::chat::{Chat, ChatPage};
use leptos::prelude::*;
use leptos_meta::{MetaTags, Stylesheet, Title, provide_meta_context};
use leptos_router::components::{Route, Router, Routes};
use leptos_router::hooks::use_location;
use leptos_router::path;

/// Modules shown in the sidebar: (label, path, icon class).
/// To add one: add an entry here and a matching `<Route>` below.
const MODULES: [(&str, &str, &str); 2] = [
    ("Chat", "/", "icon-chat"),
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
                    let active = move || location.pathname.with(|p| p == href);
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
            <div class="sidebar-foot">"Running locally with Ollama"</div>
        </nav>
    }
}
