//! Sends one real, tiny, signed-and-broadcast stagenet payment to a given
//! address - a standalone manual-debugging tool. The Playwright suite
//! itself no longer calls this (see `pos_e2e_server.rs::send_payment_handler`,
//! which does the same job in-process, serialized against that process's
//! own scan loop); kept as a separate `[[bin]]` purely for exercising
//! `stagenet-test-wallet` directly from the command line.
//!
//! ```sh
//! cargo run --features e2e -p scanner --bin pos-e2e-send-payment -- <address> <piconero_amount>
//! ```
//!
//! Run from the repository root (`e2e/stagenet-wallets.json`/
//! `e2e/stagenet-known-outputs.json`/`e2e/stagenet-decoy-distribution.json`
//! are all read relative to the current directory). Prints the broadcast
//! tx's hex-encoded hash to stdout on its own last line on success; a real,
//! actionable message to stderr (with a non-zero exit) on failure.

use serde_json::Value;

use stagenet_test_wallet::{Ledger, StagenetTestWallet, WalletError};

const NODE_HOST: &str = "node.monerodevs.org";
const NODE_PORT: u16 = 38089;
const NODE_SSL: bool = false;
const NODE_ACCEPT_SELF_SIGNED_CERTS: bool = true;
const WALLETS_PATH: &str = "e2e/stagenet-wallets.json";
const KNOWN_OUTPUTS_PATH: &str = "e2e/stagenet-known-outputs.json";
const DECOY_DISTRIBUTION_PATH: &str = "e2e/stagenet-decoy-distribution.json";

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

    // Retries the whole connect-then-send sequence from scratch on any
    // error except `WalletError::Broadcast` (a broadcast was actually
    // attempted and its outcome is genuinely unknown - retrying past it
    // risks a real double-send). Same shape
    // `pos_e2e_server.rs::send_payment_handler` uses - kept in sync,
    // deliberately duplicated rather than shared, same reasoning every
    // other real-money-costing duplication in this crate already gives.
    const SEND_ATTEMPTS: u32 = 5;
    let mut attempt = 1;
    let tx_hash = loop {
        let result = async {
            let wallet =
                StagenetTestWallet::connect(&node_url, NODE_ACCEPT_SELF_SIGNED_CERTS, &customer_spend_key_hex, &customer_view_key_hex, &customer_address, DECOY_DISTRIBUTION_PATH).await?;
            let mut ledger = Ledger::load(KNOWN_OUTPUTS_PATH)?;
            wallet.send(&mut ledger, &address, amount_piconero).await
        }
        .await;
        match result {
            Ok(hash) => break hash,
            Err(e @ WalletError::Broadcast(_)) => {
                eprintln!("broadcast outcome unknown, not retrying: {e}");
                std::process::exit(1);
            }
            Err(e) if attempt < SEND_ATTEMPTS => {
                eprintln!("attempt {attempt}/{SEND_ATTEMPTS}: retrying after: {e}");
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
    };
    println!("{}", hex::encode(tx_hash));
}
