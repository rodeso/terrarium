pub mod app;
pub mod calc;
pub mod calculator;
pub mod chat;
pub mod emoji;
pub mod markdown;

#[cfg(feature = "ssr")]
pub mod server;

/// Browser entry point: attaches interactivity to the server-rendered page.
#[cfg(feature = "hydrate")]
#[wasm_bindgen::prelude::wasm_bindgen]
pub fn hydrate() {
    console_error_panic_hook::set_once();
    leptos::mount::hydrate_body(app::Workspace);
}
