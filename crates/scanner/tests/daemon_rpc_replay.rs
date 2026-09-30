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
use scanner::daemon::{ChainBlock, KeyImageStatus, MoneroDaemonClient, TxLocation};
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
/// Well-formed, but no transaction's id.
const ABSENT_TX: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

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
    assert!(client.get_height().await.unwrap() >= KNOWN_TX_HEIGHT);
    // The tip's height comes with its id, in one answer.
    let tip = client.get_tip().await.unwrap();
    assert!(tip.height >= KNOWN_TX_HEIGHT, "tip {}", tip.height);
    assert_eq!(tip.hash.as_ref().map(String::len), Some(64), "{tip:?}");

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

    // What the scanner reads blocks with: the same chunk with pruned
    // transactions. Each block names itself and its parent, and each
    // transaction carries the id its whole form hashes to, with the prefix
    // (output keys, key images) a scan reads unchanged.
    let chain = client.get_chain_blocks(START, COUNT).await.unwrap();
    assert_eq!(chain.len(), COUNT as usize);
    let mut pruned_bytes = 0;
    let mut whole_bytes = 0;
    for (offset, block) in chain.iter().enumerate() {
        let height = START + offset as u64;
        assert_eq!(block.height, height);
        assert_eq!(block.hash, client.get_block_hash(height).await.unwrap());
        if offset > 0 {
            assert_eq!(block.prev_hash, chain[offset - 1].hash);
        }
        let whole = &batched[offset];
        assert_eq!(block.txs.len(), whole.len(), "block {height}");
        for (index, (pruned, whole)) in block.txs.iter().zip(whole).enumerate() {
            assert_eq!(block.txid(index), Some(tx_id_hex(whole)), "block {height}");
            assert!(shared::monero_tx::is_pruned(pruned), "block {height}");
            assert_eq!(pruned.prefix, whole.prefix);
            assert_eq!(pruned.rct_signatures.sig, whole.rct_signatures.sig);
            pruned_bytes += serialize(pruned).len();
            whole_bytes += serialize(whole).len();
        }
    }
    assert!(
        pruned_bytes * 3 < whole_bytes,
        "pruned {pruned_bytes} of {whole_bytes} bytes"
    );

    // Headers alone say the same about each block as the blocks do.
    let headers = client.get_chain_headers(START, COUNT).await.unwrap();
    assert_eq!(
        headers,
        chain.iter().map(ChainBlock::header).collect::<Vec<_>>()
    );
    // A range past the node's tip gives what there is, not an error.
    let at_tip = client.get_chain_headers(tip.height, 5).await.unwrap();
    assert_eq!(at_tip[0].height, tip.height);

    // Looking the transaction up directly finds the same one, mined at its height.
    assert_eq!(
        client.locate_transaction(KNOWN_TX).await.unwrap(),
        TxLocation::InBlock(KNOWN_TX_HEIGHT)
    );
    assert_eq!(
        client.locate_transaction(ABSENT_TX).await.unwrap(),
        TxLocation::NotFound
    );
    assert_eq!(
        tx_id_hex(&client.get_transaction(KNOWN_TX).await.unwrap()),
        KNOWN_TX
    );
    // Several at once: each one placed or affirmatively missed.
    let both = [KNOWN_TX.to_string(), ABSENT_TX.to_string()];
    let located = client.locate_transactions(&both).await.unwrap();
    assert_eq!(
        located.get(KNOWN_TX),
        Some(&TxLocation::InBlock(KNOWN_TX_HEIGHT))
    );
    assert_eq!(located.get(ABSENT_TX), Some(&TxLocation::NotFound));
    assert_eq!(located.len(), 2);
    // The transaction and where it is, in one answer, pruned, under its id.
    let (found, location) = client.find_transaction(KNOWN_TX).await.unwrap().unwrap();
    assert_eq!(location, TxLocation::InBlock(KNOWN_TX_HEIGHT));
    assert_eq!(found.txid, KNOWN_TX);
    assert!(shared::monero_tx::is_pruned(&found.tx));
    assert_eq!(found.tx.prefix, known.prefix);
    assert!(client.find_transaction(ABSENT_TX).await.unwrap().is_none());
    // Fetched by id: the one the node has, pruned; the other left out.
    let fetched = client.get_transactions_with_ids(&both).await.unwrap();
    assert_eq!(fetched.len(), 1);
    assert_eq!(fetched[0].txid, KNOWN_TX);
    assert_eq!(fetched[0].tx.prefix, known.prefix);

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

    let timestamp = client.get_block_timestamp(KNOWN_TX_HEIGHT).await.unwrap();
    assert!(timestamp > 1_750_000_000, "timestamp {timestamp}");
    assert_eq!(
        timestamp,
        chain[(KNOWN_TX_HEIGHT - START) as usize].timestamp
    );

    // The pool, followed by its changes: the node describes its pool in
    // answer to the wallet-style request, so the plain list of ids is never
    // asked for, and the bodies that came with the answer are handed over
    // under their ids without another request.
    let pool = client.get_mempool_txids().await.unwrap();
    let requests = |endpoint: &str| {
        client
            .stats()
            .iter()
            .find(|stats| stats.endpoint == endpoint)
            .map_or(0, |stats| stats.requests)
    };
    assert_eq!(requests("/get_blocks.bin (pool changes)"), 1);
    assert_eq!(requests("/get_transaction_pool_hashes"), 0);
    let fetches_before = requests("/get_transactions");
    let bodies = client.get_transactions_with_ids(&pool).await.unwrap();
    assert!(bodies.len() <= pool.len());
    assert!(bodies.iter().all(|body| pool.contains(&body.txid)));
    if pool.len() <= 100 {
        assert_eq!(bodies.len(), pool.len(), "every body came with the pool");
        assert_eq!(requests("/get_transactions"), fetches_before);
    }
    // Asked again at once, the last answer stands: no request.
    assert_eq!(client.get_mempool_txids().await.unwrap().len(), pool.len());
    assert_eq!(requests("/get_blocks.bin (pool changes)"), 1);
    // Whatever is in the pool decodes whole too (possibly nothing).
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
    assert!(client.get_chain_blocks(START, COUNT).await.is_err());
    assert!(client.get_chain_headers(START, COUNT).await.is_err());
    assert!(client.get_tip().await.is_err());
    // The pool: neither the request for its changes nor the plain list is
    // answered, and that is an error, never an empty pool.
    assert!(client.get_mempool_txids().await.is_err());
    assert!(client
        .locate_transactions(&[ABSENT_TX.to_string()])
        .await
        .is_err());
    assert!(client.find_transaction(ABSENT_TX).await.is_err());
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
