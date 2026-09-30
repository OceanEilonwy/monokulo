//! `RpcDaemonClient` against a recording of a real stagenet `monerod`: the
//! scanner's own client, its JSON and binary (`get_blocks.bin`, epee) wire
//! formats, and the blocks, transactions and key images a real node returned,
//! replayed from `tests/fixtures/stagenet_node_recording.json` without a
//! network. The live tests in `src/daemon_rpc.rs` need a mainnet node and are
//! `#[ignore]`d, so without this nothing in CI talks to a node-shaped server.
//!
//! `record_stagenet_node` (ignored) makes the recording through a local proxy
//! to a public stagenet node; re-run it only if the client's requests change:
//! `cargo test -p scanner --test daemon_rpc_replay -- --ignored`.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{StatusCode, Uri};
use axum::Router;
use monero::consensus::serialize;
use monero::TxIn;
use scanner::daemon::{KeyImageStatus, MoneroDaemonClient, TxLocation};
use scanner::daemon_rpc::RpcDaemonClient;
use scanner::scanner::tx_id_hex;
use serde::{Deserialize, Serialize};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/stagenet_node_recording.json"
);
const NODE: &str = "http://node2.monerodevs.org:38089";
/// Four consecutive stagenet blocks; the first transaction below is in the second.
const START: u64 = 2_210_330;
const COUNT: u64 = 4;
/// A stagenet transaction from the e2e wallets (`e2e/wallets/spender.json`).
const KNOWN_TX: &str = "098e6e358b7d66a9e85a211d6917d1b8d720e74c6ae267d4d9b10ed422819bb2";
const KNOWN_TX_HEIGHT: u64 = 2_210_331;
/// Well-formed but never spent.
const UNSPENT_KEY_IMAGE: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[derive(Serialize, Deserialize, Clone)]
struct Exchange {
    path: String,
    request_hex: String,
    response_hex: String,
}

async fn serve(router: Router) -> (u16, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (port, task)
}

/// Serves each recorded response for the same path and request body.
async fn replay() -> (RpcDaemonClient, tokio::task::JoinHandle<()>) {
    let exchanges: Vec<Exchange> =
        serde_json::from_str(&std::fs::read_to_string(FIXTURE).unwrap()).unwrap();
    let table: Arc<HashMap<(String, String), String>> = Arc::new(
        exchanges
            .into_iter()
            .map(|e| ((e.path, e.request_hex), e.response_hex))
            .collect(),
    );
    let router =
        Router::new()
            .fallback(
                |State(table): State<Arc<HashMap<(String, String), String>>>,
                 uri: Uri,
                 body: Bytes| async move {
                    match table.get(&(uri.path().to_string(), hex::encode(&body))) {
                        Some(response) => (StatusCode::OK, hex::decode(response).unwrap()),
                        None => (
                            StatusCode::NOT_FOUND,
                            format!(
                                "no recording for {} {}",
                                uri.path(),
                                String::from_utf8_lossy(&body)
                            )
                            .into_bytes(),
                        ),
                    }
                },
            )
            .with_state(table);
    let (port, task) = serve(router).await;
    (
        RpcDaemonClient::new("127.0.0.1", port, false, false).unwrap(),
        task,
    )
}

fn key_images(tx: &monero::Transaction) -> Vec<String> {
    tx.prefix
        .inputs
        .iter()
        .filter_map(|input| match input {
            TxIn::ToKey { k_image, .. } => Some(hex::encode(serialize(k_image))),
            _ => None,
        })
        .collect()
}

/// Every call the scanner makes, as it makes them. Returns what the recorder
/// needs to know about the chain.
async fn exercise(client: &RpcDaemonClient) {
    let tip = client.get_height().await.unwrap();
    assert!(tip >= KNOWN_TX_HEIGHT, "tip {tip}");

    // One get_blocks.bin round trip for the chunk matches fetching each block
    // with get_block + get_transactions, transaction for transaction.
    let batched = client.get_blocks_range(START, COUNT).await.unwrap();
    assert_eq!(batched.len(), COUNT as usize);
    for (offset, block) in batched.iter().enumerate() {
        let single = client
            .get_block_transactions(START + offset as u64)
            .await
            .unwrap();
        assert_eq!(
            block.iter().map(tx_id_hex).collect::<Vec<_>>(),
            single.iter().map(tx_id_hex).collect::<Vec<_>>(),
            "block {}",
            START + offset as u64
        );
    }
    let known_block = &batched[(KNOWN_TX_HEIGHT - START) as usize];
    let known = known_block
        .iter()
        .find(|tx| tx_id_hex(tx) == KNOWN_TX)
        .expect("known transaction in its block");

    // Looking the transaction up directly finds the same one, mined at its height.
    assert_eq!(
        client.locate_transaction(KNOWN_TX).await.unwrap(),
        TxLocation::InBlock(KNOWN_TX_HEIGHT)
    );
    assert_eq!(
        tx_id_hex(&client.get_transaction(KNOWN_TX).await.unwrap()),
        KNOWN_TX
    );

    // Its inputs' key images are spent on chain; a made-up one is not.
    let mut images = key_images(known);
    assert!(!images.is_empty());
    images.push(UNSPENT_KEY_IMAGE.to_string());
    let statuses = client.is_key_image_spent(&images).await.unwrap();
    assert_eq!(statuses.last(), Some(&KeyImageStatus::Unspent));
    assert!(
        statuses[..statuses.len() - 1]
            .iter()
            .all(|s| *s == KeyImageStatus::SpentInBlockchain),
        "{statuses:?}"
    );

    let hash = client.get_block_hash(KNOWN_TX_HEIGHT).await.unwrap();
    assert_eq!(hash.len(), 64);
    let timestamp = client.get_block_timestamp(KNOWN_TX_HEIGHT).await.unwrap();
    assert!(timestamp > 1_750_000_000, "timestamp {timestamp}");
    // Whatever is in the pool decodes (possibly nothing).
    client.get_mempool_transactions().await.unwrap();
}

#[tokio::test]
async fn scanner_node_client_reads_blocks_transactions_and_key_images_from_a_recorded_stagenet_node(
) {
    let (client, _server) = replay().await;
    exercise(&client).await;
}

#[tokio::test]
async fn scanner_node_client_reports_a_node_that_does_not_answer_as_it_expects() {
    // A node (or something in front of it) answering with an error page, not
    // monerod's JSON: every call fails with a readable error rather than
    // being read as empty results.
    let router = Router::new()
        .fallback(|| async { (StatusCode::BAD_GATEWAY, "<html>502 Bad Gateway</html>") });
    let (port, _server) = serve(router).await;
    let client = RpcDaemonClient::new("127.0.0.1", port, false, false).unwrap();
    let error = client.get_height().await.unwrap_err().to_string();
    assert!(error.contains("invalid JSON response"), "{error}");
    let error = client.get_block_hash(1).await.unwrap_err().to_string();
    assert!(error.contains("invalid JSON response"), "{error}");
    assert!(client.get_blocks_range(START, COUNT).await.is_err());
    assert!(client
        .is_key_image_spent(&[UNSPENT_KEY_IMAGE.to_string()])
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "records from a public stagenet node; run by hand when the client's requests change"]
async fn record_stagenet_node() {
    let recorded: Arc<Mutex<Vec<Exchange>>> = Arc::default();
    let http = reqwest::Client::new();
    let router = Router::new()
        .fallback(
            |State((recorded, http)): State<(Arc<Mutex<Vec<Exchange>>>, reqwest::Client)>,
             uri: Uri,
             body: Bytes| async move {
                let response = http
                    .post(format!("{NODE}{}", uri.path()))
                    .body(body.clone())
                    .send()
                    .await
                    .unwrap()
                    .bytes()
                    .await
                    .unwrap();
                recorded.lock().push(Exchange {
                    path: uri.path().to_string(),
                    request_hex: hex::encode(&body),
                    response_hex: hex::encode(&response),
                });
                (StatusCode::OK, response)
            },
        )
        .with_state((recorded.clone(), http));
    let (port, _server) = serve(router).await;
    exercise(&RpcDaemonClient::new("127.0.0.1", port, false, false).unwrap()).await;
    let exchanges = recorded.lock().clone();
    std::fs::write(FIXTURE, serde_json::to_string_pretty(&exchanges).unwrap()).unwrap();
}
