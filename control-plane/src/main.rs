//! Thin binary wrapper — all real logic lives in the library (`src/lib.rs`),
//! same split as the engine's own `main.rs`/`lib.rs`. No config file, CLI
//! parsing, or deployment wiring yet (that's for a later WBS task); this is
//! just enough to actually run the one endpoint that exists so far.

use control_plane::db::Db;
use control_plane::engine_client::EngineClient;
use control_plane::http::{AppState, build_router};

#[tokio::main]
async fn main() {
    let db = Db::open_file("control_plane.db").expect("failed to open control-plane database").into_shared();
    // TODO: real config. No config-file system exists yet in control-plane
    // (a later WBS task adds one); until then, this is a placeholder engine
    // URL, not a real deployment wiring.
    let engine_client = EngineClient::new("http://127.0.0.1:8080");
    let app_state = AppState { db, engine_client };
    let router = build_router(app_state);

    let bind = "127.0.0.1:8081";
    let listener = tokio::net::TcpListener::bind(bind).await.expect("failed to bind server address");
    println!("control-plane listening on {bind}");
    axum::serve(listener, router).await.expect("server error");
}
