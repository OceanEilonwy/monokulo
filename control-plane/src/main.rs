//! Thin binary wrapper — all real logic lives in the library (`src/lib.rs`),
//! same split as the engine's own `main.rs`/`lib.rs`. No config file, CLI
//! parsing, or deployment wiring yet (that's for a later WBS task); this is
//! just enough to actually run the one endpoint that exists so far.

use control_plane::db::Db;
use control_plane::engine_client::EngineClient;
use control_plane::http::status_page::new_status_cache;
use control_plane::http::{AppState, build_router};
use control_plane::templates::TemplateEngine;

/// Reads the AES-256-GCM key (WBS 1.2.3) used to encrypt the engine's
/// `sk_...` secret token at rest (see `control_plane::crypto`) from
/// `CONTROL_PLANE_ENCRYPTION_KEY`, expected as 64 hex characters (32 bytes).
///
/// Deliberately no hardcoded fallback key: unlike the engine-URL placeholder
/// above (a stub value for a service that isn't really deployed yet), a
/// checked-in "temporary" encryption key would be a real credential leak the
/// moment this ever runs against a real database. A clear startup panic
/// telling the operator exactly what to set is the right placeholder
/// behavior instead.
fn encryption_key_from_env() -> [u8; 32] {
    let hex_key = std::env::var("CONTROL_PLANE_ENCRYPTION_KEY").expect(
        "CONTROL_PLANE_ENCRYPTION_KEY must be set to 64 hex characters (32 bytes) - \
         e.g. generate one with `openssl rand -hex 32`",
    );
    let bytes = hex::decode(&hex_key).expect(
        "CONTROL_PLANE_ENCRYPTION_KEY must be valid hex (64 hex characters decoding to exactly 32 bytes)",
    );
    <[u8; 32]>::try_from(bytes.as_slice())
        .expect("CONTROL_PLANE_ENCRYPTION_KEY must decode to exactly 32 bytes (64 hex characters)")
}

#[tokio::main]
async fn main() {
    let db = Db::open_file("control_plane.db").expect("failed to open control-plane database").into_shared();
    // TODO: real config. No config-file system exists yet in control-plane
    // (a later WBS task adds one); until then, this is a placeholder engine
    // URL, not a real deployment wiring.
    let engine_client = EngineClient::new("http://127.0.0.1:8080");
    let encryption_key = encryption_key_from_env();
    let templates =
        std::sync::Arc::new(TemplateEngine::new().expect("built-in signup/login templates must parse"));
    let app_state = AppState { db, engine_client, encryption_key, templates, status_cache: new_status_cache() };
    let router = build_router(app_state);

    let bind = "127.0.0.1:8081";
    let listener = tokio::net::TcpListener::bind(bind).await.expect("failed to bind server address");
    println!("control-plane listening on {bind}");
    axum::serve(listener, router).await.expect("server error");
}
