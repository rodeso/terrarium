#[cfg(feature = "ssr")]
#[actix_web::main]
async fn main() -> std::io::Result<()> {
    use actix_files::Files;
    use actix_web::{App, HttpServer, middleware::Logger, web};
    use leptos::config::get_configuration;
    use leptos_actix::{LeptosRoutes, generate_route_list};
    use terrarium::app::{App, shell};
    use terrarium::chat::MAX_UPLOAD_BYTES;
    use terrarium::server::AppState;

    let conf = get_configuration(None).expect("failed to read Leptos configuration");
    let addr = conf.leptos_options.site_addr;
    let state = web::Data::new(AppState::new().await);

    println!("Listening on http://{addr}");

    HttpServer::new(move || {
        let routes = generate_route_list(App);
        let leptos_options = conf.leptos_options.clone();
        let site_root = leptos_options.site_root.to_string();

        App::new()
            .wrap(Logger::default())
            .app_data(state.clone())
            // Server function bodies are read in one go; allow room for a 2 MB
            // file plus JSON escaping.
            .app_data(web::PayloadConfig::new(MAX_UPLOAD_BYTES * 2 + 64 * 1024))
            .service(Files::new("/pkg", format!("{site_root}/pkg")))
            .leptos_routes(routes, {
                let leptos_options = leptos_options.clone();
                move || shell(leptos_options.clone())
            })
            .app_data(web::Data::new(leptos_options))
            // Static files from `public/` (e.g. favicon); must come last.
            .service(Files::new("/", site_root))
    })
    .bind(addr)?
    .run()
    .await
}

#[cfg(not(feature = "ssr"))]
fn main() {
    // The binary is only built with the `ssr` feature; the browser side
    // starts from `hydrate()` in lib.rs.
}
