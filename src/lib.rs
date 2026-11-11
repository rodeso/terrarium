pub mod app;
pub mod calc;
pub mod calendar;
pub mod calculator;
pub mod chat;
pub mod dates;
pub mod emoji;
pub mod markdown;
pub mod news;
pub mod notes;

#[cfg(feature = "ssr")]
pub mod db;
#[cfg(feature = "ssr")]
pub mod feeds;
#[cfg(feature = "ssr")]
pub mod server;

/// Browser entry point: attaches interactivity to the server-rendered page.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::App);
}
