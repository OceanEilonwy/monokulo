//! Writes the API's reference to the folder named on the command line:
//! `examples.json`, `openapi.json` and `reference.json`
//! (`monokulo::api::runtime::recording`), from exchanges really made
//! against a test engine, each checked against the OpenAPI document.
//!
//! `cargo xtask api openapi --out FILE` and `cargo xtask pages api` run it,
//! with `MONOKULO_POS_BUILD=none`, so it builds neither monokulo's tests nor
//! its browser code.

use std::path::PathBuf;

#[tokio::main]
async fn main() {
    let Some(dir) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: api_reference DIR");
        std::process::exit(2);
    };
    let engine =
        engine_test_support::spawn_test_engine_with_networks(&[monero::Network::Mainnet]).await;
    if let Err(e) = monokulo::api::runtime::recording::write(engine.router(), &dir).await {
        eprintln!(
            "the API reference couldn't be written to {}: {e}",
            dir.display()
        );
        std::process::exit(1);
    }
}
