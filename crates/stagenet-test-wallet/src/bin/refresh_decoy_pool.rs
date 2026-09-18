//! Refreshes the committed decoy-distribution snapshot `DecoyCache` (see
//! `../lib.rs`) serves every send from, instead of a live fetch each time.
//!
//! Run occasionally, by hand - never by the e2e suites themselves, and
//! never automatically. A stale snapshot is still fully valid input (see
//! `DecoyCache`'s own doc comment for why), so there's no correctness
//! reason to run this often; the only reason to re-run it at all is to keep
//! decoy candidates reasonably close to the current chain rather than
//! arbitrarily old.
//!
//! ```sh
//! cargo run -p stagenet-test-wallet --bin refresh-decoy-pool -- \
//!   <node_url> <from_height> <to_height> <out_path>
//! # e.g., from the repository root:
//! cargo run -p stagenet-test-wallet --bin refresh-decoy-pool -- \
//!   http://node.monerodevs.org:38089 0 1700000 e2e/stagenet-decoy-distribution.json
//! ```
//!
//! `to_height` should be comfortably behind the live tip (this fetch itself
//! can take several seconds against a real public node - the exact thing
//! `DecoyCache` exists to avoid paying on every send) and `from_height` is
//! typically `0` (whole-chain history) unless deliberately narrowing the
//! snapshot to keep the committed file smaller.

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let usage = "usage: refresh-decoy-pool <node_url> <from_height> <to_height> <out_path>";
    let node_url = args.next().unwrap_or_else(|| {
        eprintln!("{usage}");
        std::process::exit(2);
    });
    let from: usize = args
        .next()
        .unwrap_or_else(|| {
            eprintln!("{usage}");
            std::process::exit(2);
        })
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("from_height must be a plain integer: {e}");
            std::process::exit(2);
        });
    let to: usize = args
        .next()
        .unwrap_or_else(|| {
            eprintln!("{usage}");
            std::process::exit(2);
        })
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("to_height must be a plain integer: {e}");
            std::process::exit(2);
        });
    let out_path = args.next().unwrap_or_else(|| {
        eprintln!("{usage}");
        std::process::exit(2);
    });

    match stagenet_test_wallet::refresh_decoy_distribution(&node_url, true, from, to, &out_path).await {
        Ok(len) => println!("wrote {len} distribution entries to {out_path}"),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
