//! Sends one real, tiny, signed-and-broadcast stagenet payment to a given
//! address - the second half of the POS screen's real e2e suite
//! (`e2e/pos-playwright/`), invoked as a child process from the Playwright
//! test right after it reads a real order's address off the real POS page
//! (rendered by `pos-e2e-server`, see that binary's own doc comment). Reuses
//! the exact same `scanner::e2e_wallet::StagenetSpendWallet` (real CLSAG +
//! Bulletproofs+ signing, no external wallet-rpc process) and the same
//! `e2e/stagenet-wallets.json` customer wallet `tests/e2e_stagenet.rs`/
//! `tests/e2e_dashboard_stagenet.rs` already use - deliberately never
//! reimplemented in JS; Playwright only ever shells out to this, it never
//! touches key material itself.
//!
//! ```sh
//! cargo run --features e2e -p scanner --bin pos-e2e-send-payment -- <address> <piconero_amount>
//! ```
//!
//! Run from the repository root (`e2e/stagenet-wallets.json` is read/written
//! relative to the current directory, same convention every other real
//! stagenet test/binary in this crate follows). Prints the broadcast tx's
//! hex-encoded hash to stdout on its own last line on success; a real,
//! actionable message to stderr (with a non-zero exit) on failure - no
//! customer wallet has ever run dry mid-run without a clear message per
//! `e2e/README.md`'s own "reproducing from scratch" section, and this
//! binary follows that same contract rather than a bare panic.

use serde_json::{json, Value};

use scanner::daemon::MoneroDaemonClient;
use scanner::daemon_rpc::RpcDaemonClient;
use scanner::e2e_wallet::StagenetSpendWallet;

const NODE_HOST: &str = "node.monerodevs.org";
const NODE_PORT: u16 = 38089;
const NODE_SSL: bool = false;
const NODE_ACCEPT_SELF_SIGNED_CERTS: bool = true;
const WALLETS_PATH: &str = "e2e/stagenet-wallets.json";

/// Deliberately duplicated from `tests/e2e_dashboard_stagenet.rs::record_known_txid`
/// rather than shared - see that function's own doc comment for why (never
/// touch another real-money-costing caller's own copy while editing this
/// one). Same atomic temp-file-then-rename write-back.
fn record_known_txid(tx_hash: &str) {
    let mut wallets_json: Value =
        serde_json::from_str(&std::fs::read_to_string(WALLETS_PATH).unwrap_or_else(|e| panic!("failed to read {WALLETS_PATH}: {e}")))
            .unwrap_or_else(|e| panic!("failed to parse {WALLETS_PATH}: {e}"));
    let known = wallets_json["customer"]["known_txids"].as_array_mut().expect("customer.known_txids must be an array");
    if !known.iter().any(|v| v.as_str() == Some(tx_hash)) {
        known.push(json!(tx_hash));
    }
    let tmp_path = format!("{WALLETS_PATH}.tmp");
    std::fs::write(&tmp_path, serde_json::to_string_pretty(&wallets_json).unwrap() + "\n").unwrap_or_else(|e| panic!("failed to write {tmp_path}: {e}"));
    std::fs::rename(&tmp_path, WALLETS_PATH).unwrap_or_else(|e| panic!("failed to move {tmp_path} into place over {WALLETS_PATH}: {e}"));
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| {
        eprintln!("usage: pos-e2e-send-payment <destination_address> <piconero_amount>");
        std::process::exit(2);
    });
    let amount_piconero: u64 = args
        .next()
        .unwrap_or_else(|| {
            eprintln!("usage: pos-e2e-send-payment <destination_address> <piconero_amount>");
            std::process::exit(2);
        })
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("piconero_amount must be a plain integer: {e}");
            std::process::exit(2);
        });

    let node_url = format!("http{}://{NODE_HOST}:{NODE_PORT}", if NODE_SSL { "s" } else { "" });
    let wallets_json: Value =
        serde_json::from_str(&std::fs::read_to_string(WALLETS_PATH).unwrap_or_else(|e| panic!("failed to read {WALLETS_PATH}: {e}")))
            .unwrap_or_else(|e| panic!("failed to parse {WALLETS_PATH}: {e}"));
    let customer_address = wallets_json["customer"]["address"].as_str().expect("customer.address missing").to_string();
    let customer_spend_key_hex = wallets_json["customer"]["private_spend_key"].as_str().expect("customer.private_spend_key missing").to_string();
    let customer_view_key_hex = wallets_json["customer"]["private_view_key"].as_str().expect("customer.private_view_key missing").to_string();
    let known_txids: Vec<String> = wallets_json["customer"]["known_txids"]
        .as_array()
        .expect("customer.known_txids missing")
        .iter()
        .map(|v| v.as_str().expect("known_txids entries must be strings").to_string())
        .collect();

    let daemon = RpcDaemonClient::new(NODE_HOST, NODE_PORT, NODE_SSL, NODE_ACCEPT_SELF_SIGNED_CERTS).expect("failed to build daemon RPC client");
    if let Err(e) = daemon.get_height().await {
        eprintln!("cannot reach the stagenet node at {NODE_HOST}:{NODE_PORT}: {e}");
        std::process::exit(1);
    }

    let spend_wallet = StagenetSpendWallet::connect(&node_url, NODE_ACCEPT_SELF_SIGNED_CERTS, &customer_spend_key_hex, &customer_view_key_hex, &customer_address)
        .await
        .unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(1);
        });
    let tx_hash = spend_wallet.send(&daemon, &known_txids, &address, amount_piconero).await.unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    let tx_hash_hex = hex::encode(tx_hash);
    record_known_txid(&tx_hash_hex);
    println!("{tx_hash_hex}");
}
