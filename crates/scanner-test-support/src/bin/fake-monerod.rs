//! A tiny stand-in for monerod, for end-to-end tests that run the real
//! engine binary offline (admin_settings_v2.md task 6.0).
//!
//! It serves a fixed chain of empty blocks and an empty mempool - enough for
//! the engine to configure a network, tick healthily and report it on its
//! status page. It can't produce payments: that would need a
//! `get_blocks.bin` encoder and real transactions, so payment flows are
//! tested in-process with `FakeDaemonClient` instead. (With no
//! `get_blocks.bin`, the engine reads its pool from the plain list of ids,
//! as from a monerod too old to describe its pool's changes.)
//!
//! Usage: `fake-monerod [--port N] [--height N] [--nettype NAME]`, where
//! `--nettype` is the network `get_info` reports (default `stagenet`, what
//! the Playwright suite uses; a test starts one with `mainnet` to see a
//! node on the wrong network refused). Prints
//! `FAKE_MONEROD_READY <address>` once listening. `POST /fake/offline` and
//! `POST /fake/online` make it refuse or serve every other request, so a
//! test can take "the node" down and bring it back.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{json, Value};

#[derive(Clone)]
struct Chain {
    /// Block count, as monerod's `/get_height` reports it.
    count: Arc<AtomicU64>,
    online: Arc<AtomicBool>,
    /// The network `get_info` reports.
    nettype: Arc<str>,
}

impl Chain {
    /// `get_info`'s result: the fields the engine reads, and the flags an
    /// older monerod sends instead of `nettype`.
    fn info(&self) -> Value {
        json!({
            "height": self.count.load(Ordering::SeqCst),
            "nettype": &*self.nettype,
            "mainnet": &*self.nettype == "mainnet",
            "stagenet": &*self.nettype == "stagenet",
            "testnet": &*self.nettype == "testnet",
            "status": "OK",
        })
    }
}

/// A deterministic 64-hex-character hash per height.
fn block_hash(height: u64) -> String {
    format!("{height:016x}").repeat(4)
}

impl Chain {
    fn check(&self) -> Result<(), Box<Response>> {
        if self.online.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(Box::new(
                (StatusCode::SERVICE_UNAVAILABLE, "offline").into_response(),
            ))
        }
    }
}

async fn get_height(State(chain): State<Chain>) -> Response {
    if let Err(r) = chain.check() {
        return *r;
    }
    // The chain's length and, as monerod does, its top block's id.
    let count = chain.count.load(Ordering::SeqCst);
    Json(json!({ "height": count, "hash": block_hash(count.saturating_sub(1)), "status": "OK" }))
        .into_response()
}

/// A block's header, as `get_block_header_by_height` and
/// `get_block_headers_range` give it.
fn block_header(height: u64) -> Value {
    json!({
        "hash": block_hash(height),
        "prev_hash": height.checked_sub(1).map(block_hash).unwrap_or_else(|| "0".repeat(64)),
        "height": height,
        "timestamp": 1_700_000_000 + height * 120,
    })
}

async fn json_rpc(State(chain): State<Chain>, Json(request): Json<Value>) -> Response {
    if let Err(r) = chain.check() {
        return *r;
    }
    let id = request.get("id").cloned().unwrap_or(json!("0"));
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let top = chain.count.load(Ordering::SeqCst).saturating_sub(1);
    match method {
        "get_info" => {
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": chain.info() })).into_response()
        }
        "get_block_header_by_height" => {
            let height = request
                .pointer("/params/height")
                .and_then(Value::as_u64)
                .unwrap_or(top);
            if height > top {
                return too_high(id, height, top);
            }
            let result = json!({ "block_header": block_header(height), "status": "OK" });
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
        }
        "on_get_block_hash" => {
            let height = request
                .pointer("/params/0")
                .and_then(Value::as_u64)
                .unwrap_or(top);
            if height > top {
                return too_high(id, height, top);
            }
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": block_hash(height) }))
                .into_response()
        }
        // Like monerod, a range that runs past the tip is refused whole.
        "get_block_headers_range" => {
            let bound = |name: &str| {
                request
                    .pointer(&format!("/params/{name}"))
                    .and_then(Value::as_u64)
                    .unwrap_or(top)
            };
            let (start, end) = (bound("start_height"), bound("end_height"));
            if end > top || start > end {
                return too_high(id, end, top);
            }
            let headers: Vec<Value> = (start..=end).map(block_header).collect();
            Json(json!({ "jsonrpc": "2.0", "id": id, "result": { "headers": headers, "status": "OK" } }))
                .into_response()
        }
        other => {
            eprintln!("fake-monerod: unsupported json_rpc method {other:?}");
            Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } })).into_response()
        }
    }
}

fn too_high(id: Value, height: u64, top: u64) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -2, "message": format!("requested height {height} greater than current top block height {top}") } })).into_response()
}

async fn get_info(State(chain): State<Chain>) -> Response {
    if let Err(r) = chain.check() {
        return *r;
    }
    Json(chain.info()).into_response()
}

async fn empty_pool_hashes(State(chain): State<Chain>) -> Response {
    if let Err(r) = chain.check() {
        return *r;
    }
    Json(json!({ "tx_hashes": [], "status": "OK" })).into_response()
}

async fn no_transactions(State(chain): State<Chain>) -> Response {
    if let Err(r) = chain.check() {
        return *r;
    }
    Json(json!({ "txs": [], "missed_tx": [], "status": "OK" })).into_response()
}

async fn set_online(State(chain): State<Chain>) -> StatusCode {
    chain.online.store(true, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

async fn set_offline(State(chain): State<Chain>) -> StatusCode {
    chain.online.store(false, Ordering::SeqCst);
    StatusCode::NO_CONTENT
}

async fn unsupported(uri: axum::http::Uri) -> StatusCode {
    eprintln!("fake-monerod: unsupported request {uri}");
    StatusCode::NOT_FOUND
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let port = match arg("--port").map(u16::try_from) {
        None => 0,
        Some(Ok(port)) => port,
        Some(Err(_)) => {
            eprintln!("fake-monerod: --port must be at most 65535");
            std::process::exit(2);
        }
    };
    let height = arg("--height").unwrap_or(1000);
    let nettype = args
        .iter()
        .position(|a| a == "--nettype")
        .and_then(|i| args.get(i + 1))
        .map_or("stagenet", String::as_str);
    let chain = Chain {
        count: Arc::new(AtomicU64::new(height + 1)),
        online: Arc::new(AtomicBool::new(true)),
        nettype: Arc::from(nettype),
    };
    let app = Router::new()
        .route("/get_height", post(get_height))
        .route("/get_info", post(get_info).get(get_info))
        .route("/json_rpc", post(json_rpc))
        .route("/get_transaction_pool_hashes", post(empty_pool_hashes))
        .route("/get_transactions", post(no_transactions))
        .route("/fake/online", post(set_online))
        .route("/fake/offline", post(set_offline))
        .fallback(unsupported)
        .with_state(chain);
    let listener = match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("fake-monerod: can't listen on port {port}: {e}");
            std::process::exit(1);
        }
    };
    match listener.local_addr() {
        Ok(address) => println!("FAKE_MONEROD_READY {address}"),
        Err(e) => {
            eprintln!("fake-monerod: {e}");
            std::process::exit(1);
        }
    }
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("fake-monerod: {e}");
        std::process::exit(1);
    }
}
