//! Real `MoneroDaemonClient` implementation, talking to `monerod`'s actual RPC
//! surface over `reqwest` + `rustls`. Every endpoint and field name used here was
//! verified against a live public node (`node.hollingworth.xyz:18089`, restricted
//! RPC) before being written, the same way `key_custody`'s crypto was verified
//! against `monero-rs`'s own source rather than assumed from memory - see the
//! `#[ignore]`d tests at the bottom of this file for the live proof.
//!
//! Three RPC surfaces are in play: the JSON-RPC envelope at `/json_rpc` (block
//! hashes and headers, `get_info`), monerod's "other" plain-JSON endpoints
//! (`/get_height`, `/get_transaction_pool_hashes`, `/get_transactions`,
//! `/is_key_image_spent`) which are not wrapped in a `jsonrpc`/`result`
//! envelope at all, and the binary (epee) `/get_blocks.bin`, for blocks and
//! for changes to the mempool.
//!
//! What is asked for is kept small (docs/node_rpc_efficiency.md):
//! transactions come pruned (the prefix and RingCT base a scan reads, about a
//! sixth of the bytes), a block's id comes from the block itself, a lone hash
//! or header is asked for as just that, and the mempool is followed by its
//! changes rather than re-listed. [`RpcDaemonClient::stats`] counts every
//! request and its bytes, by endpoint.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use monero::consensus::encode::deserialize;
use monero::cryptonote::hash::Hashable;
use monero::Transaction;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::daemon::{
    ChainBlock, ChainHeader, ChainTip, DaemonError, DaemonInfo, FetchedTx, KeyImageStatus,
    MoneroDaemonClient, TxLocation,
};

pub struct RpcDaemonClient {
    client: reqwest::Client,
    base_url: String,
    /// Largest response body accepted (task 7.6). A node that sends more is
    /// treated as failing rather than being allowed to exhaust memory.
    max_response_bytes: usize,
    stats: parking_lot::Mutex<HashMap<String, EndpointStats>>,
    /// The mempool as this node last described it. An async lock: one poll
    /// at a time, held across its round trip, so two loops polling at once
    /// can't apply the same changes out of order.
    pool: tokio::sync::Mutex<PoolView>,
}

/// Requests made to one endpoint of a node (a path, or a JSON-RPC method),
/// and the bytes they cost: [`RpcDaemonClient::stats`].
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct EndpointStats {
    pub endpoint: String,
    /// Requests sent, answered or not.
    pub requests: u64,
    /// Request body bytes.
    pub bytes_sent: u64,
    /// Response body bytes read.
    pub bytes_received: u64,
}

/// Default cap on one response body: comfortably above a full mempool under
/// load or a `get_blocks.bin` chunk sized by `payment.scan_chunk_memory_budget_mb`,
/// far below what would hurt the machine.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Reads a response body, refusing one larger than `cap` bytes, whether or
/// not it declared its length.
async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
    what: &str,
) -> Result<Vec<u8>, DaemonError> {
    if response
        .content_length()
        .is_some_and(|len| len > cap as u64)
    {
        return Err(DaemonError::Request(format!(
            "response from {what} is larger than the {cap}-byte limit"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| DaemonError::Request(format!("reading response from {what}: {e}")))?
    {
        if body.len() + chunk.len() > cap {
            return Err(DaemonError::Request(format!(
                "response from {what} is larger than the {cap}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

impl RpcDaemonClient {
    /// `danger_accept_invalid_certs` exists because many community-run public
    /// Monero nodes (this project's own test node included) serve a self-signed
    /// TLS certificate - not a bug in this client, just the reality of who runs
    /// public Monero infrastructure. This constructor takes no position on the
    /// default; `MoneroNodeConfig::accept_self_signed_certs` does, and it defaults
    /// **on** for the reason above, with `--strict-tls` (see `main::Args`) as the
    /// override that forces it off for every configured node. Note this is
    /// `reqwest`'s blunt flag underneath: it also tolerates expired and
    /// wrong-hostname certificates, so it is not scoped to self-signed
    /// specifically. A self-hoster pointing at a node with a real CA-signed cert
    /// (or running their own node over plain HTTP on localhost) never needs it.
    pub fn new(
        host: &str,
        port: u16,
        ssl: bool,
        danger_accept_invalid_certs: bool,
    ) -> Result<Self, DaemonError> {
        let scheme = if ssl { "https" } else { "http" };
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(danger_accept_invalid_certs)
            .timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| DaemonError::Request(format!("failed to build HTTP client: {e}")))?;
        Ok(RpcDaemonClient {
            client,
            base_url: format!("{scheme}://{host}:{port}"),
            max_response_bytes: MAX_RESPONSE_BYTES,
            stats: Default::default(),
            pool: Default::default(),
        })
    }

    /// Every endpoint asked since this client was built, busiest (by bytes
    /// received) first.
    pub fn stats(&self) -> Vec<EndpointStats> {
        let mut stats: Vec<EndpointStats> = self.stats.lock().values().cloned().collect();
        stats.sort_by(|a, b| {
            b.bytes_received
                .cmp(&a.bytes_received)
                .then_with(|| a.endpoint.cmp(&b.endpoint))
        });
        stats
    }

    /// Sends one request body to `path` and reads the (capped) response,
    /// counting both under `endpoint`.
    async fn post(
        &self,
        endpoint: &str,
        path: &str,
        body: Vec<u8>,
        json: bool,
    ) -> Result<Vec<u8>, DaemonError> {
        let sent = body.len() as u64;
        {
            let mut stats = self.stats.lock();
            let entry = stats
                .entry(endpoint.to_string())
                .or_insert_with(|| EndpointStats {
                    endpoint: endpoint.to_string(),
                    ..Default::default()
                });
            entry.requests += 1;
            entry.bytes_sent += sent;
        }
        let mut request = self.client.post(format!("{}{path}", self.base_url));
        if json {
            request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        }
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|e| DaemonError::Request(e.to_string()))?;
        let bytes = read_capped(response, self.max_response_bytes, endpoint).await?;
        if let Some(entry) = self.stats.lock().get_mut(endpoint) {
            entry.bytes_received += bytes.len() as u64;
        }
        Ok(bytes)
    }

    /// Lowers the response size cap, for tests.
    pub fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
        self.max_response_bytes = max_response_bytes;
        self
    }

    async fn post_json_rpc<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
    ) -> Result<T, DaemonError> {
        let body = json!({ "jsonrpc": "2.0", "id": "0", "method": method, "params": params });
        let bytes = self
            .post(method, "/json_rpc", body.to_string().into_bytes(), true)
            .await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| DaemonError::Request(format!("invalid JSON response: {e}")))?;
        if let Some(err) = value.get("error") {
            return Err(DaemonError::Request(format!(
                "daemon RPC error calling {method}: {err}"
            )));
        }
        let result = value.get("result").ok_or_else(|| {
            DaemonError::Request(format!("missing 'result' field calling {method}"))
        })?;
        serde_json::from_value(result.clone())
            .map_err(|e| DaemonError::Request(format!("failed to parse result of {method}: {e}")))
    }

    async fn post_plain<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: Value,
    ) -> Result<T, DaemonError> {
        let bytes = self
            .post(path, path, body.to_string().into_bytes(), true)
            .await?;
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| DaemonError::Request(format!("invalid JSON response from {path}: {e}")))?;
        if let Some(status) = value.get("status").and_then(|s| s.as_str()) {
            if status != "OK" {
                return Err(DaemonError::Request(format!(
                    "daemon returned status {status} from {path}"
                )));
            }
        }
        // A field-shape mismatch here means the response was valid JSON but didn't
        // match what this client expects - a different node/version genuinely
        // shaping a response differently (see `GetTransactionPoolResponse`'s own
        // note), not a network problem. Including a snippet of the actual body in
        // the error is what makes that diagnosable from the error message alone,
        // rather than needing to reproduce it with a packet capture.
        serde_json::from_value(value.clone()).map_err(|e| {
            let full = value.to_string();
            // `String` slicing panics off a char boundary; `char_indices` finds the
            // nearest safe cut at or before 500 bytes rather than assuming ASCII.
            let cut = full
                .char_indices()
                .map(|(i, _)| i)
                .take_while(|&i| i <= 500)
                .last()
                .unwrap_or(0);
            let snippet = if full.len() > cut {
                format!("{}…", &full[..cut])
            } else {
                full
            };
            DaemonError::Request(format!(
                "failed to parse response from {path}: {e}\nresponse body was: {snippet}"
            ))
        })
    }

    /// Posts a raw (non-JSON) body to one of monerod's binary `.bin` endpoints and
    /// returns the raw response bytes, unparsed - the epee wire format
    /// (`get_blocks_range`'s own request/response, below) has nothing to do with
    /// `post_json_rpc`/`post_plain`'s JSON envelopes. Relies on the same
    /// `reqwest::Client` (and its 15s timeout, set once in `new`) every other
    /// call on this client already does, and the same `max_response_bytes`
    /// cap: a timeout alone doesn't bound size, since a fast node can send a
    /// lot in 15s.
    async fn post_bin(&self, path: &str, body: Vec<u8>) -> Result<Vec<u8>, DaemonError> {
        self.post(path, path, body, false).await
    }

    /// `/get_transactions` for `hashes`: whole transactions, or (`prune`)
    /// each one's prefix and RingCT base with the hash of the rest.
    async fn request_transactions(
        &self,
        hashes: &[String],
        prune: bool,
    ) -> Result<GetTransactionsResponse, DaemonError> {
        let mut body = json!({ "txs_hashes": hashes, "decode_as_json": false });
        if prune {
            body["prune"] = json!(true);
        }
        self.post_plain("/get_transactions", body).await
    }

    async fn fetch_transactions(&self, hashes: &[String]) -> Result<Vec<Transaction>, DaemonError> {
        if hashes.is_empty() {
            return Ok(vec![]);
        }
        let resp = self.request_transactions(hashes, false).await?;
        decode_all_or_fail(hashes, resp)
    }

    /// The real `get_blocks.bin` call, always with `start_height >= 1` (the
    /// public `get_blocks_range` override handles the height-0 special case
    /// before ever calling this). Whole transactions: its callers hash them.
    async fn get_blocks_bin_range(
        &self,
        start_height: u64,
        max_block_count: u64,
    ) -> Result<Vec<Vec<Transaction>>, DaemonError> {
        self.get_blocks_bin(start_height, max_block_count, false)
            .await?
            .into_iter()
            .map(|block| {
                block
                    .txs
                    .iter()
                    .map(|tx| {
                        deserialize(&tx.blob).map_err(|e| {
                            DaemonError::Request(format!(
                                "failed to parse a transaction blob from get_blocks.bin: {e}"
                            ))
                        })
                    })
                    .collect()
            })
            .collect()
    }

    async fn get_blocks_bin(
        &self,
        start_height: u64,
        max_block_count: u64,
        prune: bool,
    ) -> Result<Vec<BinBlock>, DaemonError> {
        let request = get_blocks_bin_request(start_height, max_block_count, prune);
        let response = self.post_bin("/get_blocks.bin", request).await?;
        parse_get_blocks_bin_response(&response)
    }

    /// One header by height (`get_block_header_by_height`): about a
    /// kilobyte, where `get_block` sends the whole block's hashes too.
    async fn block_header(&self, height: u64) -> Result<BlockHeader, DaemonError> {
        #[derive(Deserialize)]
        struct HeaderResult {
            block_header: BlockHeader,
        }
        let result: HeaderResult = self
            .post_json_rpc("get_block_header_by_height", json!({ "height": height }))
            .await?;
        Ok(result.block_header)
    }
}

/// Turns a `/get_transactions` response into transactions, refusing to return fewer
/// than were asked for.
///
/// A node that can't produce every requested transaction must be an error, never a
/// shorter list. `fetch_transactions`' caller is enumerating a *block's* contents:
/// silently returning 86 of a block's 87 transactions doesn't degrade the scan, it
/// makes the scan wrong - the missing transaction is one nobody will ever look at
/// again, because the block gets marked scanned either way. If it paid a customer's
/// order, that payment is simply never detected, and nothing anywhere logs a
/// complaint. Two realistic ways to land here: a pruned node with no blob for an
/// older transaction, and a reorg between the `get_block` that produced these hashes
/// and this call. Both should stall the scan for a tick, not lose a payment.
///
/// Split out as a free function purely so this can be tested without standing up an
/// HTTP server - the response shape is the whole of the logic worth pinning.
fn decode_all_or_fail(
    hashes: &[String],
    resp: GetTransactionsResponse,
) -> Result<Vec<Transaction>, DaemonError> {
    if !resp.missed_tx.is_empty() {
        return Err(DaemonError::Request(format!(
            "daemon could not supply {} of {} requested transactions (first missing: {}) - \
             node may be pruned, or the chain moved between calls",
            resp.missed_tx.len(),
            hashes.len(),
            resp.missed_tx[0],
        )));
    }
    let entries = resp.txs.unwrap_or_default();
    if entries.len() != hashes.len() {
        return Err(DaemonError::Request(format!(
            "daemon returned {} transactions for {} requested hashes",
            entries.len(),
            hashes.len()
        )));
    }
    entries
        .iter()
        .map(|entry| decode_tx_hex(&entry.as_hex))
        .collect()
}

/// Turns a single-hash `/get_transactions` response into a [`TxLocation`], insisting
/// that the daemon actually answered the question before reporting `NotFound`.
///
/// `NotFound` is not an ordinary "no" here - it is the single most consequential
/// value this whole client can return. `scanner::check_for_reorg_and_reconcile` reacts
/// to it by asking `is_key_image_spent` about that payment's inputs and, on
/// `SpentInBlockchain`, **voiding the payment and stamping a double-spend on the
/// order**. That inference is only valid because `NotFound` is supposed to mean the
/// transaction is in no block and no mempool: if it really is gone, then something
/// *else* must have spent its inputs. `/is_key_image_spent` reports only a status
/// code, never a spending txid, so there is no independent corroboration anywhere -
/// the entire double-spend conclusion rests on this one classification being right.
/// Get it wrong for a transaction that is actually confirmed and the key images come
/// back `SpentInBlockchain` *because that very transaction spent them*, and a
/// perfectly good confirmed payment is written off.
///
/// So only an affirmative answer counts. `missed_tx` naming the hash is the daemon
/// saying "I do not have this"; that is `NotFound`. Everything else that fails to
/// place the transaction is a *non-answer* and becomes an error:
///
///  - Neither list mentions the hash. Every field of `GetTransactionsResponse` is
///    optional (necessarily - a real node omits `txs` when nothing matched and
///    `missed_tx` when nothing was missed), so a bare `{"status":"OK"}` from anything
///    sitting between this client and `monerod` deserializes cleanly into
///    "no transactions, nothing missed" and used to become `NotFound`. This
///    deployment explicitly expects public nodes behind load balancers whose backends
///    disagree with each other - see `FakeDaemonClient::height_override`, which exists
///    because that was observed live - so a response from a backend that has nothing
///    to say is not hypothetical.
///  - An entry that is not in the pool and carries no `block_height`: the daemon has
///    claimed the transaction is confirmed and simultaneously declined to say where.
///
/// An error costs a retry and nothing else: reconciliation aborts for this tick with
/// the stored block hashes untouched, so the next tick re-detects the same reorg and
/// re-reconciles from scratch - the property
/// `scanner::tests::a_reconciliation_that_fails_partway_leaves_the_reorg_still_detectable`
/// pins. Trading a retry for never voiding a live payment off a response that never
/// said it was gone is not a close call.
fn classify_located_transaction(
    txid: &str,
    resp: GetTransactionsResponse,
) -> Result<TxLocation, DaemonError> {
    if resp.missed_tx.iter().any(|h| h == txid) {
        return Ok(TxLocation::NotFound);
    }
    match resp.txs.and_then(|v| v.into_iter().next()) {
        Some(entry) if entry.in_pool => Ok(TxLocation::InPool),
        Some(entry) => match entry.block_height {
            Some(h) => Ok(TxLocation::InBlock(h)),
            None => Err(DaemonError::Request(format!(
                "daemon reported transaction {txid} as confirmed but gave no block_height - \
                 refusing to read that as 'not on the chain', which would void the payment"
            ))),
        },
        None => Err(DaemonError::Request(format!(
            "daemon returned neither a transaction nor a miss for {txid} - refusing to read a \
             non-answer as 'not on the chain', which would void the payment"
        ))),
    }
}

/// Decodes the mempool, skipping (and logging) entries that won't parse rather than
/// failing the whole poll.
///
/// This is deliberately the *opposite* policy to `decode_all_or_fail`, and the
/// difference is not inconsistency - it's that the two calls have opposite failure
/// consequences. A block is scanned exactly once and then marked scanned forever, so
/// silently dropping one of its transactions loses whatever payment it contained; all
/// or nothing is the only safe reading there. The mempool is re-polled every second,
/// nothing about it is ever marked done, and a transaction this build cannot
/// deserialize cannot be matched against a wallet no matter how many times it is
/// retried. So failing the whole call over one bad entry buys nothing and costs
/// everything: `run_scan_tick` gets an error instead of a mempool, and zero-conf
/// detection is off for *every* tenant on this network for as long as that
/// transaction sits in the pool - which, since anyone can put a transaction in a
/// public mempool and it lingers for days, is a trivially reachable state, not a
/// hypothetical one. The payment is still detected the moment it is mined; only its
/// zero-conf sighting is lost, and only for the one transaction that could not be
/// read.
fn decode_pool_best_effort(entries: &[PoolTx]) -> Vec<Transaction> {
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        match decode_tx_hex(&entry.tx_blob) {
            Ok(tx) => out.push(tx),
            Err(e) => tracing::warn!(
                error = %e,
                "skipping one undecodable mempool transaction - it cannot be matched against any wallet either \
                 way, and failing the whole poll over it would disable zero-conf detection for every tenant on \
                 this network"
            ),
        }
    }
    out
}

fn decode_tx_hex(hex_str: &str) -> Result<Transaction, DaemonError> {
    if hex_str.is_empty() {
        return Err(DaemonError::Request(
            "transaction has no as_hex data (likely pruned on this node)".to_string(),
        ));
    }
    let bytes =
        hex::decode(hex_str).map_err(|e| DaemonError::Request(format!("invalid tx hex: {e}")))?;
    deserialize(&bytes)
        .map_err(|e| DaemonError::Request(format!("failed to parse transaction blob: {e}")))
}

/// One field of an epee request object.
enum EpeeField {
    Bool(bool),
    U8(u8),
    U64(u64),
}

/// An epee-encoded request: a flat object of `fields`, in order.
///
/// The encoding: an 8-byte magic header, a 1-byte version, then the object
/// as a compact-varint field count followed by `(1-byte name length, name
/// bytes, 1-byte type tag, value)` per field - see `monero_epee`'s own
/// module docs for the full format. A count under 64 fits the varint's
/// 1-byte form (`count << 2`), which is all this ever needs.
#[allow(
    clippy::expect_used,
    reason = "only called with a few short field-name literals"
)]
fn epee_request(fields: &[(&str, EpeeField)]) -> Vec<u8> {
    let mut request = Vec::with_capacity(96);
    request.extend_from_slice(&monero_epee::HEADER);
    request.push(monero_epee::VERSION);
    let count = u8::try_from(fields.len()).expect("more request fields than fit a u8");
    assert!(count < 64, "more request fields than the 1-byte varint holds");
    request.push(count << 2);
    for (name, value) in fields {
        request.push(u8::try_from(name.len()).expect("field name literal longer than 255 bytes"));
        request.extend_from_slice(name.as_bytes());
        #[expect(clippy::as_conversions)]
        match value {
            EpeeField::Bool(value) => {
                request.push(monero_epee::Type::Bool as u8);
                request.push(u8::from(*value));
            }
            EpeeField::U8(value) => {
                request.push(monero_epee::Type::Uint8 as u8);
                request.push(*value);
            }
            EpeeField::U64(value) => {
                request.push(monero_epee::Type::Uint64 as u8);
                request.extend_from_slice(&value.to_le_bytes());
            }
        }
    }
    request
}

/// A `get_blocks.bin` request for blocks: `prune` (bool), `start_height`
/// (uint64) and `max_block_count` (uint64) - the request shape monerod
/// expects for this endpoint (confirmed against `monero-daemon-rpc`'s own,
/// separately-published implementation of the same call, `bin_rpc/blocks_bin.
/// rs`'s `fetch_contiguous_blocks`, and against live nodes).
///
/// With `prune`, each transaction comes as its prefix and RingCT base (what
/// a scan reads) with the hash of the rest, from which its id is checked
/// against the block's own list (`BinBlock::into_chain_block`).
fn get_blocks_bin_request(start_height: u64, max_block_count: u64, prune: bool) -> Vec<u8> {
    epee_request(&[
        ("prune", EpeeField::Bool(prune)),
        ("start_height", EpeeField::U64(start_height)),
        ("max_block_count", EpeeField::U64(max_block_count)),
    ])
}

/// `get_blocks.bin`'s `requested_info` for "the pool only, no blocks".
const REQUESTED_INFO_POOL_ONLY: u8 = 2;
/// `pool_info_extent` values: every pool transaction, or only the changes
/// since `pool_info_since`. (0, or no field at all: the node said nothing
/// about the pool - one too old to know the request.)
const POOL_INFO_INCREMENTAL: u8 = 1;
const POOL_INFO_FULL: u8 = 2;

/// A `get_blocks.bin` request for what changed in the pool since `since`
/// (a `daemon_time` from an earlier answer; 0 for the whole pool), with
/// the added transactions pruned. The request wallets poll with.
fn pool_changes_request(since: u64) -> Vec<u8> {
    epee_request(&[
        ("requested_info", EpeeField::U8(REQUESTED_INFO_POOL_ONLY)),
        ("pool_info_since", EpeeField::U64(since)),
        ("prune", EpeeField::Bool(true)),
    ])
}

/// One transaction of a `get_blocks.bin` block entry, undecoded.
struct BinTx {
    blob: Vec<u8>,
    /// The hash of the part a pruned blob leaves out, when the node sent it.
    prunable_hash: Option<[u8; 32]>,
}

/// One entry of a `get_blocks.bin` response: the block blob (header, miner
/// transaction and transaction hashes) and the transactions themselves.
struct BinBlock {
    block: Option<Vec<u8>>,
    txs: Vec<BinTx>,
}

impl BinBlock {
    /// The block's identity from its own blob, with its transactions and
    /// their ids.
    ///
    /// Everything is checked against the blob, whose hash is the block's id:
    /// the height (the coinbase names it), so a node answering from another
    /// height is refused rather than its blocks scanned as the ones asked
    /// for; and each transaction's id against the block's own list, so what
    /// is scanned is what the block holds.
    fn into_chain_block(self, height: u64) -> Result<ChainBlock, DaemonError> {
        let blob = self.block.ok_or_else(|| {
            DaemonError::Request(format!("get_blocks.bin: block {height} had no block blob"))
        })?;
        let block: monero::Block = deserialize(&blob).map_err(|e| {
            DaemonError::Request(format!(
                "get_blocks.bin: block {height} could not be decoded: {e}"
            ))
        })?;
        match block.miner_tx.prefix.inputs.first() {
            Some(monero::blockdata::transaction::TxIn::Gen { height: own }) if own.0 == height => {}
            Some(monero::blockdata::transaction::TxIn::Gen { height: own }) => {
                return Err(DaemonError::Request(format!(
                    "get_blocks.bin: asked for block {height}, the node sent block {}",
                    own.0
                )))
            }
            _ => {
                return Err(DaemonError::Request(format!(
                    "get_blocks.bin: block {height} has no coinbase input"
                )))
            }
        }
        if block.tx_hashes.len() != self.txs.len() {
            return Err(DaemonError::Request(format!(
                "get_blocks.bin: block {height} lists {} transactions but {} came with it",
                block.tx_hashes.len(),
                self.txs.len()
            )));
        }
        let mut txs = Vec::with_capacity(self.txs.len());
        let mut txids = Vec::with_capacity(self.txs.len());
        for (entry, listed) in self.txs.iter().zip(&block.tx_hashes) {
            let txid = hex::encode(listed.0);
            let tx = decode_tx_blob(&entry.blob, entry.prunable_hash.as_ref(), Some(&txid))
                .map_err(|e| {
                    DaemonError::Request(format!("get_blocks.bin: block {height}: {e}"))
                })?;
            txs.push(tx.tx);
            txids.push(txid);
        }
        Ok(ChainBlock {
            height,
            hash: hex::encode(block.id().0),
            prev_hash: hex::encode(block.header.prev_id.0),
            timestamp: block.header.timestamp.0,
            txs,
            txids: Some(txids),
        })
    }
}

/// A transaction blob, whole or pruned, as a transaction with its id.
///
/// The id is computed wherever it can be: a whole transaction hashes to it,
/// and a pruned version 2 one does together with `prunable_hash`. If the
/// node (or the block the transaction came in) `claimed` an id, a computed
/// one that differs is an error: the transaction isn't the one named. Only
/// where nothing can be computed (a pruned version 1 transaction, or no
/// prunable hash sent) is the claimed id taken as given.
fn decode_tx_blob(
    blob: &[u8],
    prunable_hash: Option<&[u8; 32]>,
    claimed: Option<&str>,
) -> Result<FetchedTx, String> {
    let tx = shared::monero_tx::decode_any(blob)
        .map_err(|e| format!("failed to parse a transaction blob: {e}"))?;
    let computed = if shared::monero_tx::is_pruned(&tx) {
        // An all-zero hash is a node saying it has none, not a hash.
        prunable_hash
            .filter(|hash| **hash != [0; 32])
            .and_then(|hash| shared::monero_tx::pruned_txid(&tx, hash))
    } else {
        Some(tx.hash())
    }
    .map(|hash| hex::encode(hash.to_bytes()));
    let txid = match (computed, claimed) {
        (Some(computed), Some(claimed)) if computed != claimed => {
            return Err(format!(
                "a transaction sent as {claimed} hashes to {computed}"
            ))
        }
        (Some(computed), _) => computed,
        (None, Some(claimed)) if is_txid(claimed) => claimed.to_string(),
        (None, _) => return Err("a pruned transaction came without an id".to_string()),
    };
    Ok(FetchedTx { txid, tx })
}

/// Whether `s` has the shape of a transaction id: 64 lowercase hex digits.
fn is_txid(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// A 32-byte value from an epee string field, if it is one.
fn hash32(bytes: &[u8]) -> Option<[u8; 32]> {
    bytes.try_into().ok()
}

fn epee_err(what: &str, e: monero_epee::EpeeError) -> DaemonError {
    DaemonError::Request(format!("invalid {what} response: {e:?}"))
}

/// Parses a `get_blocks.bin` response into its block entries, in the order
/// monerod returned them (ascending height, since the request asked for a
/// contiguous range starting at a fixed height). Nothing is decoded here:
/// each entry keeps its block blob and its transaction blobs. Un-consumed
/// fields (`pruned`, `block_weight`, `output_indices`, ...) are skipped
/// automatically by `monero_epee`'s own `Drop`-based cursor advance - see its
/// own module docs.
fn parse_get_blocks_bin_response(bytes: &[u8]) -> Result<Vec<BinBlock>, DaemonError> {
    let epee_err = |e| epee_err("get_blocks.bin", e);

    let mut epee = monero_epee::Epee::new(bytes).map_err(epee_err)?;
    let mut fields = epee.entry().map_err(epee_err)?.fields().map_err(epee_err)?;

    let mut status: Option<Vec<u8>> = None;
    let mut blocks: Option<Vec<BinBlock>> = None;

    while let Some(entry) = fields.next() {
        let (key, value) = entry.map_err(epee_err)?;
        match key.consume() {
            b"status" => {
                status = Some(value.to_str().map_err(epee_err)?.consume().to_vec());
            }
            b"blocks" => {
                let mut block_entries = value.iterate().map_err(epee_err)?;
                let mut out = Vec::new();
                while let Some(block_entry) = block_entries.next() {
                    let mut block_fields =
                        block_entry.map_err(epee_err)?.fields().map_err(epee_err)?;
                    let mut txs = Vec::new();
                    let mut block_blob = None;
                    while let Some(field) = block_fields.next() {
                        let (field_key, field_value) = field.map_err(epee_err)?;
                        match field_key.consume() {
                            b"txs" => {}
                            // The header, coinbase and transaction hashes: the
                            // block's identity.
                            b"block" => {
                                block_blob = Some(
                                    field_value.to_str().map_err(epee_err)?.consume().to_vec(),
                                );
                                continue;
                            }
                            _ => continue,
                        }
                        // `txs`' own element shape differs, confirmed against
                        // real responses: a plain `Array<String>` (each element
                        // the raw tx blob) for whole transactions, and
                        // `Array<Object{blob, prunable_hash}>` for pruned ones
                        // (older nodes: strings there too) - so both are
                        // handled, dispatched on the array's declared element
                        // type rather than assuming either.
                        let element_kind = field_value.kind();
                        let mut tx_entries = field_value.iterate().map_err(epee_err)?;
                        while let Some(tx_entry) = tx_entries.next() {
                            let tx_entry = tx_entry.map_err(epee_err)?;
                            let tx = match element_kind {
                                monero_epee::Type::String => BinTx {
                                    blob: tx_entry.to_str().map_err(epee_err)?.consume().to_vec(),
                                    prunable_hash: None,
                                },
                                monero_epee::Type::Object => {
                                    let mut tx_fields = tx_entry.fields().map_err(epee_err)?;
                                    let mut blob: Option<Vec<u8>> = None;
                                    let mut prunable_hash = None;
                                    while let Some(tx_field) = tx_fields.next() {
                                        let (tx_field_key, tx_field_value) =
                                            tx_field.map_err(epee_err)?;
                                        match tx_field_key.consume() {
                                            b"blob" => {
                                                blob = Some(
                                                    tx_field_value
                                                        .to_str()
                                                        .map_err(epee_err)?
                                                        .consume()
                                                        .to_vec(),
                                                );
                                            }
                                            b"prunable_hash" => {
                                                prunable_hash = hash32(
                                                    tx_field_value
                                                        .to_str()
                                                        .map_err(epee_err)?
                                                        .consume(),
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                    BinTx {
                                        blob: blob.ok_or_else(|| {
                                            DaemonError::Request(
                                                "get_blocks.bin: a tx entry (object form) had no blob field".to_string(),
                                            )
                                        })?,
                                        prunable_hash,
                                    }
                                }
                                other => {
                                    return Err(DaemonError::Request(format!(
                                        "get_blocks.bin: txs array held an unexpected element type {other:?}"
                                    )));
                                }
                            };
                            txs.push(tx);
                        }
                    }
                    out.push(BinBlock {
                        block: block_blob,
                        txs,
                    });
                }
                blocks = Some(out);
            }
            _ => {}
        }
    }

    match status {
        Some(ref s) if s == b"OK" => {}
        Some(s) => {
            return Err(DaemonError::Request(format!(
                "get_blocks.bin returned status {:?}",
                std::string::String::from_utf8_lossy(&s)
            )));
        }
        None => {
            return Err(DaemonError::Request(
                "get_blocks.bin response had no status field".to_string(),
            ))
        }
    }

    blocks.ok_or_else(|| {
        DaemonError::Request("get_blocks.bin response had no blocks field".to_string())
    })
}

/// What a node said changed in its pool (`get_blocks.bin` with
/// `requested_info` = pool only).
#[derive(Debug, Default, PartialEq)]
struct PoolChanges {
    /// Every pool transaction (`true`), or only what changed since the
    /// request's `pool_info_since`.
    full: bool,
    /// The node's clock at the answer: the next request's
    /// `pool_info_since`.
    daemon_time: u64,
    /// Transactions that entered the pool, with their (pruned) bodies.
    added: Vec<(String, Vec<u8>)>,
    /// Transactions that entered the pool, ids only (a restricted node
    /// sends at most 100 bodies per answer).
    added_ids: Vec<String>,
    removed: Vec<String>,
}

/// Parses a pool-only `get_blocks.bin` response. `Ok(None)` when the node
/// answered without describing its pool: one that doesn't know the request
/// (older than monerod 0.18.3), to be asked the old way instead.
fn parse_pool_changes(bytes: &[u8]) -> Result<Option<PoolChanges>, DaemonError> {
    let epee_err = |e| epee_err("pool get_blocks.bin", e);
    fn ids(blob: &[u8]) -> Vec<String> {
        blob.chunks_exact(32).map(hex::encode).collect()
    }

    let mut epee = monero_epee::Epee::new(bytes).map_err(epee_err)?;
    let mut fields = epee.entry().map_err(epee_err)?.fields().map_err(epee_err)?;
    let mut status: Option<Vec<u8>> = None;
    let mut extent: Option<u8> = None;
    let mut daemon_time: Option<u64> = None;
    let mut changes = PoolChanges::default();
    while let Some(entry) = fields.next() {
        let (key, value) = entry.map_err(epee_err)?;
        match key.consume() {
            b"status" => status = Some(value.to_str().map_err(epee_err)?.consume().to_vec()),
            b"pool_info_extent" => extent = Some(value.to_u8().map_err(epee_err)?),
            b"daemon_time" => daemon_time = Some(value.to_u64().map_err(epee_err)?),
            b"remaining_added_pool_txids" => {
                changes.added_ids = ids(value.to_str().map_err(epee_err)?.consume());
            }
            b"removed_pool_txids" => {
                changes.removed = ids(value.to_str().map_err(epee_err)?.consume());
            }
            b"added_pool_txs" => {
                let mut entries = value.iterate().map_err(epee_err)?;
                while let Some(entry) = entries.next() {
                    let mut tx_fields = entry.map_err(epee_err)?.fields().map_err(epee_err)?;
                    let (mut txid, mut blob) = (None, None);
                    while let Some(field) = tx_fields.next() {
                        let (field_key, field_value) = field.map_err(epee_err)?;
                        match field_key.consume() {
                            b"tx_hash" => {
                                txid = hash32(field_value.to_str().map_err(epee_err)?.consume())
                                    .map(hex::encode);
                            }
                            b"tx_blob" => {
                                blob = Some(
                                    field_value.to_str().map_err(epee_err)?.consume().to_vec(),
                                );
                            }
                            _ => {}
                        }
                    }
                    match (txid, blob) {
                        (Some(txid), Some(blob)) => changes.added.push((txid, blob)),
                        // Known to be in the pool, fetched like any other.
                        (Some(txid), None) => changes.added_ids.push(txid),
                        (None, _) => {
                            return Err(DaemonError::Request(
                                "pool get_blocks.bin: an added transaction had no id".to_string(),
                            ))
                        }
                    }
                }
            }
            _ => {}
        }
    }
    if status.as_deref() != Some(b"OK") {
        return Ok(None);
    }
    changes.full = match extent {
        Some(POOL_INFO_FULL) => true,
        Some(POOL_INFO_INCREMENTAL) => false,
        _ => return Ok(None),
    };
    let Some(daemon_time) = daemon_time else {
        return Ok(None);
    };
    changes.daemon_time = daemon_time;
    Ok(Some(changes))
}

/// How long one answer about the pool stands in for asking again. The fast
/// mempool loop and the round's tier poll the same node a fraction of a
/// second apart; a second poll this soon after the first gets its answer.
const POOL_REUSE: Duration = Duration::from_millis(100);
/// How often the pool as followed by its changes is checked against the
/// node's plain list of ids, so a missed change (a node restarted, a load
/// balancer's backends disagreeing) is corrected within this long.
const POOL_RESYNC_INTERVAL: Duration = Duration::from_secs(60);
/// How long a node that didn't describe its pool is asked the old way
/// before it is tried again (it may have been upgraded).
const POOL_CHANGES_RETRY: Duration = Duration::from_secs(10 * 60);
/// Most transaction bodies that arrived with pool changes kept until they
/// are asked for. Past this (a flood) they are fetched on demand instead.
const POOL_BODIES_MAX: usize = 5_000;

/// The pool as one node last described it, kept between polls so each poll
/// asks only for what changed.
#[derive(Default)]
struct PoolView {
    /// Whether the node answers requests for pool changes: not yet known,
    /// yes, or no (asked the old way until `retry_changes_at`).
    follows_changes: Option<bool>,
    retry_changes_at: Option<Instant>,
    /// The node's clock at its last answer.
    since: u64,
    txids: HashSet<String>,
    /// Bodies that arrived with the changes and haven't been asked for yet.
    bodies: HashMap<String, Transaction>,
    polled_at: Option<Instant>,
    resynced_at: Option<Instant>,
}

impl PoolView {
    fn apply(&mut self, changes: PoolChanges) {
        if changes.full {
            self.txids.clear();
            self.bodies.clear();
        }
        for (txid, blob) in changes.added {
            // A body that doesn't decode is left to be fetched (and
            // reported) by whoever asks for it.
            if self.bodies.len() < POOL_BODIES_MAX {
                if let Ok(tx) = shared::monero_tx::decode_any(&blob) {
                    self.bodies.insert(txid.clone(), tx);
                }
            }
            self.txids.insert(txid);
        }
        self.txids.extend(changes.added_ids);
        for txid in &changes.removed {
            self.txids.remove(txid);
            self.bodies.remove(txid);
        }
        self.since = changes.daemon_time;
    }

    /// Replaces the ids with the node's plain list, keeping the bodies of
    /// those still there.
    fn resync(&mut self, txids: Vec<String>) {
        self.txids = txids.into_iter().collect();
        let txids = &self.txids;
        self.bodies.retain(|txid, _| txids.contains(txid));
    }

    fn list(&self) -> Vec<String> {
        self.txids.iter().cloned().collect()
    }
}

/// Matches `COMMAND_RPC_GET_HEIGHT::response_t`: `uint64_t height`, plain
/// `KV_SERIALIZE` (always present), and `hash`, the tip block's id, read in
/// the same instant (optional here: something in front of a node may drop
/// it, and the height alone is still an answer).
#[derive(Deserialize)]
struct GetHeightResponse {
    height: u64,
    #[serde(default)]
    hash: Option<String>,
}

/// `get_info`'s network fields. Newer monerod says `nettype` outright;
/// older ones only set one of the `mainnet`/`stagenet`/`testnet` flags.
/// Every field is optional: a node that sends none of them is "unknown".
#[derive(Deserialize)]
struct GetInfoResult {
    /// The block count, as `/get_height` has it: one more than the tip's
    /// height.
    #[serde(default)]
    height: Option<u64>,
    #[serde(default)]
    nettype: Option<String>,
    #[serde(default)]
    mainnet: bool,
    #[serde(default)]
    stagenet: bool,
    #[serde(default)]
    testnet: bool,
}

impl GetInfoResult {
    fn nettype(&self) -> String {
        match &self.nettype {
            Some(nettype) if !nettype.trim().is_empty() => nettype.trim().to_ascii_lowercase(),
            _ if self.mainnet => "mainnet".to_string(),
            _ if self.stagenet => "stagenet".to_string(),
            _ if self.testnet => "testnet".to_string(),
            _ => DaemonInfo::UNKNOWN.to_string(),
        }
    }
}

/// Matches `block_header_response`'s `hash`/`timestamp` fields: `std::string hash`,
/// `uint64_t timestamp`, both plain `KV_SERIALIZE` (always present). The real
/// struct carries ~18 more always-present fields (`height`, `difficulty`, `reward`,
/// ...) nothing here reads.
#[derive(Deserialize)]
struct BlockHeader {
    hash: String,
    timestamp: u64,
    /// Absent from nothing real; optional so `get_block` answers written
    /// before this field was read here still parse in tests.
    #[serde(default)]
    height: Option<u64>,
    #[serde(default)]
    prev_hash: String,
}

impl BlockHeader {
    /// The header as the chain tier and block recorder use it. The genesis
    /// block's parent is the empty string here, not monerod's 64 zeros.
    fn into_chain_header(self, height: u64) -> ChainHeader {
        ChainHeader {
            height,
            hash: self.hash,
            prev_hash: if height == 0 {
                String::new()
            } else {
                self.prev_hash
            },
            timestamp: self.timestamp,
        }
    }
}

/// Matches `COMMAND_RPC_GET_BLOCK::response_t`: `block_header` and `tx_hashes` are
/// both declared plain `KV_SERIALIZE` in the C++ source (not `_OPT`) - but
/// `#[serde(default)]` on `tx_hashes` is deliberately kept anyway. A block with no
/// non-coinbase transactions (the empty case) has been observed, live, to omit the
/// key rather than send `[]` despite the plain (non-`_OPT`) declaration - the same
/// gap between "the C++ struct says always-serialize" and "what actually arrives
/// on the wire" that `GetTransactionPoolResponse::transactions` below was found to
/// have too. Trusting the struct declaration alone here would reintroduce exactly
/// that bug for a block instead of the mempool.
#[derive(Deserialize)]
struct GetBlockResult {
    block_header: BlockHeader,
    #[serde(default)]
    tx_hashes: Vec<String>,
}

#[derive(Deserialize)]
struct GetTransactionPoolResponse {
    // An empty mempool can come back with this key omitted entirely rather than
    // present as `[]` (observed live against a real public testnet node - the same
    // class of quirk `GetBlockResult::tx_hashes` above already works around).
    // Without `#[serde(default)]`, every poll of a genuinely empty mempool fails to
    // parse at all, which turns "no pending zero-conf payments" into "zero-conf
    // detection is silently broken on this node until something changes the
    // mempool" - a much worse failure mode than the one line this guards against.
    //
    // `COMMAND_RPC_GET_TRANSACTION_POOL::response_t` declares this plain
    // `KV_SERIALIZE(transactions)` in the C++ source too (not `_OPT`) - the struct
    // declaration alone doesn't predict whether a field can be omitted on the
    // wire, which is the whole reason every vector-typed field in this file now
    // gets `#[serde(default)]` rather than trusting each one's declaration
    // individually.
    #[serde(default)]
    transactions: Vec<PoolTx>,
}

/// Matches `tx_info::tx_blob`: `std::string tx_blob`, plain `KV_SERIALIZE`, always
/// present *within* an already-present pool entry (unlike the outer `transactions`
/// array, a `tx_info` element that exists at all reliably carries its own
/// `tx_blob` - no further defensiveness needed on a scalar field one level in).
/// The real struct has ~15 more always-present fields (`fee`, `weight`,
/// `receive_time`, `double_spend_seen`, ...) unused here.
#[derive(Deserialize)]
struct PoolTx {
    tx_blob: String,
}

#[derive(Deserialize)]
struct GetTransactionsResponse {
    txs: Option<Vec<TxEntry>>,
    #[serde(default)]
    missed_tx: Vec<String>,
}

/// Verified field-by-field against `monero-project/monero`'s own
/// `src/rpc/core_rpc_server_commands_defs.h` (`COMMAND_RPC_GET_TRANSACTIONS::entry`),
/// not assumed. Real name/type kept exactly where used (`as_hex: std::string`,
/// `in_pool: bool`, both plain `KV_SERIALIZE` - always present); the real struct
/// also always-serializes `tx_hash`, `pruned_as_hex`, `double_spend_seen`, and
/// several more fields this client has no use for, which is fine - serde ignores
/// unknown fields by default, so a struct only needs to *name* what it reads.
///
/// `block_height` is the one field here worth a real note: the real struct's
/// `KV_SERIALIZE_MAP` wraps it in `if (!this_ref.in_pool) { KV_SERIALIZE(block_height)
/// ... } else { KV_SERIALIZE(relayed) ... }` - it is a *conditionally-serialized*
/// field, entirely absent from the JSON (not `null`) whenever a transaction is
/// still in the mempool. `Option<u64>` is correct as-is for this, with no
/// `#[serde(default)]` needed: serde's derive has a documented special case for
/// fields of type exactly `Option<T>` - a missing key deserializes to `None`
/// automatically, independent of `#[serde(default)]`. Pinned, not just asserted,
/// by `an_in_pool_entry_with_no_block_height_key_at_all_deserializes_as_none` below.
///
/// With `prune` requested, `as_hex` is empty and the transaction comes as
/// `pruned_as_hex` (its prefix and RingCT base) with `prunable_hash` (the hash
/// of the rest); `tx_hash` names it either way.
#[derive(Deserialize, Default)]
struct TxEntry {
    #[serde(default)]
    as_hex: String,
    #[serde(default)]
    pruned_as_hex: String,
    #[serde(default)]
    prunable_hash: String,
    #[serde(default)]
    tx_hash: String,
    in_pool: bool,
    block_height: Option<u64>,
}

impl TxEntry {
    /// Where the node places this transaction, if the entry says: in the
    /// pool, or in a block at a height. `None` is a non-answer (confirmed,
    /// with no height), never "not found".
    fn location(&self) -> Option<TxLocation> {
        match (self.in_pool, self.block_height) {
            (true, _) => Some(TxLocation::InPool),
            (false, Some(height)) => Some(TxLocation::InBlock(height)),
            (false, None) => None,
        }
    }

    /// The transaction with its id, from whichever form the node sent.
    fn fetched(&self) -> Result<FetchedTx, DaemonError> {
        let hex_blob = if self.pruned_as_hex.is_empty() {
            &self.as_hex
        } else {
            &self.pruned_as_hex
        };
        if hex_blob.is_empty() {
            return Err(DaemonError::Request(format!(
                "transaction {} came with no data",
                self.tx_hash
            )));
        }
        let blob = hex::decode(hex_blob)
            .map_err(|e| DaemonError::Request(format!("invalid tx hex: {e}")))?;
        let prunable_hash = hex::decode(&self.prunable_hash)
            .ok()
            .and_then(|bytes| hash32(&bytes));
        let claimed = self.tx_hash.to_ascii_lowercase();
        decode_tx_blob(
            &blob,
            prunable_hash.as_ref(),
            Some(claimed.as_str()).filter(|claimed| !claimed.is_empty()),
        )
        .map_err(DaemonError::Request)
    }
}

/// The real field is `std::vector<int> spent_status` (`COMMAND_RPC_IS_KEY_IMAGE_SPENT::response_t`) -
/// a signed 32-bit C++ `int`, not the `u8` this held until this was checked against
/// source. `Vec<u8>` still parsed every status code monerod is documented to
/// actually send (0/1/2), so this wasn't reachable in practice - but it meant any
/// out-of-range value (negative, or >255) would fail deserialization outright
/// before ever reaching `is_key_image_spent`'s own `_ => Unspent` catch-all, which
/// exists specifically to degrade unrecognized codes safely rather than error.
/// `#[serde(default)]` added for consistency with `GetTransactionPoolResponse`
/// above, even though the only caller never sends an empty request (so an empty,
/// possibly-omitted response is not currently reachable) - cheap insurance against
/// the same class of bug recurring here, since the underlying "an empty vector
/// field may be omitted from the wire rather than sent as `[]`" behavior has
/// already been observed live for a structurally identical field.
#[derive(Deserialize)]
struct IsKeyImageSpentResponse {
    #[serde(default)]
    spent_status: Vec<i32>,
}

/// Most transactions asked for in one `/get_transactions` request (a
/// restricted node refuses more than 100).
const TXS_PER_REQUEST: usize = 100;
/// Most headers asked for in one `get_block_headers_range` request (a
/// restricted node refuses more than 1000).
const MAX_HEADERS_PER_REQUEST: u64 = 500;
/// The name pool-change polls are counted under in [`RpcDaemonClient::stats`]:
/// the same path as block fetches, a different request.
const POOL_CHANGES_ENDPOINT: &str = "/get_blocks.bin (pool changes)";

impl RpcDaemonClient {
    /// The pool's transaction ids, the whole list
    /// (`/get_transaction_pool_hashes`).
    async fn pool_hashes(&self) -> Result<Vec<String>, DaemonError> {
        #[derive(Deserialize)]
        struct PoolHashes {
            #[serde(default)]
            tx_hashes: Vec<String>,
        }
        let resp: PoolHashes = self
            .post_plain("/get_transaction_pool_hashes", json!({}))
            .await?;
        Ok(resp.tx_hashes)
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for RpcDaemonClient {
    async fn get_height(&self) -> Result<u64, DaemonError> {
        let resp: GetHeightResponse = self.post_plain("/get_height", json!({})).await?;
        // monerod's `/get_height` "height" field is the block *count*, not the
        // top block's own height (they differ by 1 since genesis is height 0).
        // Verified three ways, not assumed from one error message: (1) the
        // official docs (docs.getmonero.org/rpc-library/monerod-rpc) describe this
        // field verbatim as "Current length of longest chain known to daemon" -
        // language for a count, not an index; (2) monero's own C++ source
        // (src/cryptonote_core/blockchain.cpp, `get_current_blockchain_height()`,
        // the function backing both `/get_height` and `get_info`) carries a
        // warning comment reading "no getheight + gethash(height-1)" - the
        // developers' own shorthand for exactly this off-by-one; (3) live against
        // a real node, `get_block` rejected the raw value with "requested height N
        // greater than current top block height N-1". Subtracting 1 here keeps
        // this trait's `get_height()` meaning one consistent thing everywhere it's
        // used: the height of the actual current tip block, directly usable with
        // `get_block_hash`/`get_block_transactions`. Originally misdiagnosed as a
        // load-balancer inconsistency (see the defensive one-block seed margin in
        // `scanner::run_scan_tick`) before checking (1)-(3) above.
        Ok(resp.height.saturating_sub(1))
    }

    /// `/get_height` again, keeping the tip block's id it carries: the
    /// height and the id are read under one lock in monerod, so they name
    /// the same block. An id of the wrong shape is dropped, not trusted.
    async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
        let resp: GetHeightResponse = self.post_plain("/get_height", json!({})).await?;
        Ok(ChainTip {
            height: resp.height.saturating_sub(1),
            hash: resp
                .hash
                .map(|hash| hash.to_ascii_lowercase())
                .filter(|hash| is_txid(hash) && resp.height > 0),
        })
    }

    /// monerod's JSON-RPC `get_info`, through the same client (and so the
    /// same 15s timeout and response cap) as every other call.
    async fn get_info(&self) -> Result<DaemonInfo, DaemonError> {
        let result: GetInfoResult = self.post_json_rpc("get_info", json!({})).await?;
        Ok(DaemonInfo {
            nettype: result.nettype(),
            height: result.height.and_then(|count| count.checked_sub(1)),
        })
    }

    /// `on_get_block_hash`: the hash and nothing else (about a hundred
    /// bytes, where `get_block` sends the whole block to read it from).
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        let hash: String = self
            .post_json_rpc("on_get_block_hash", json!([height]))
            .await?;
        let hash = hash.to_ascii_lowercase();
        if !is_txid(&hash) {
            return Err(DaemonError::Request(format!(
                "on_get_block_hash returned something that is not a block hash for height {height}"
            )));
        }
        Ok(hash)
    }

    async fn get_block_transactions(&self, height: u64) -> Result<Vec<Transaction>, DaemonError> {
        let block: GetBlockResult = self
            .post_json_rpc("get_block", json!({ "height": height }))
            .await?;
        self.fetch_transactions(&block.tx_hashes).await
    }

    /// Overrides the trait's own one-call-per-block default with monerod's real
    /// `get_blocks.bin` - one HTTP round trip for the whole chunk, batching what
    /// would otherwise be `count` separate `get_block`+`get_transactions` round
    /// trips (`get_block_transactions` above). Written from monerod's own
    /// `get_blocks.bin` handling and the same real, published request/response
    /// shape `monero-daemon-rpc`'s `bin_rpc/blocks_bin.rs` already uses for this
    /// exact endpoint against real nodes (that crate's own client isn't reused
    /// directly - see this crate's `Cargo.toml` for why - only the wire format
    /// its code confirms is real) - not independently verified against a live
    /// node in this change; `live_node_tests::real_node_get_blocks_range_matches_
    /// get_block_transactions_for_the_same_range` below exists to do exactly that
    /// (`cargo test --ignored daemon_rpc::`) before this ships to a real node.
    async fn get_blocks_range(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<Vec<Transaction>>, DaemonError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        // `get_blocks.bin`'s `start_height` field is only observed by monerod if
        // non-zero - a request for height 0 is otherwise silently treated as
        // "unset" and answered from monerod's normal chain-sync starting point
        // instead. Fetch the genesis block the ordinary way, then batch whatever
        // is left starting at height 1 - the one real-world case
        // `rescan_start_height`'s own "saturates at genesis" behavior can produce.
        if start_height == 0 {
            let mut out = vec![self.get_block_transactions(0).await?];
            if count > 1 {
                out.extend(self.get_blocks_bin_range(1, count - 1).await?);
            }
            return Ok(out);
        }
        self.get_blocks_bin_range(start_height, count).await
    }

    /// `get_blocks.bin`, decoding each block's own header: one round trip
    /// for the range, and every block's id computed from the same blob its
    /// transactions came with. Genesis the ordinary way, as for
    /// `get_blocks_range`.
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let mut from = start_height;
        if start_height == 0 {
            // One `get_block` names the genesis block, dates it and lists
            // its transactions.
            let block: GetBlockResult = self
                .post_json_rpc("get_block", json!({ "height": 0 }))
                .await?;
            let txs = self.fetch_transactions(&block.tx_hashes).await?;
            out.push(ChainBlock {
                height: 0,
                hash: block.block_header.hash,
                prev_hash: String::new(),
                timestamp: block.block_header.timestamp,
                txs,
                txids: None,
            });
            from = 1;
        }
        let wanted = count - out.len() as u64;
        if wanted > 0 {
            let blocks = self.get_blocks_bin(from, wanted, true).await?;
            for (offset, block) in blocks.into_iter().take(wanted as usize).enumerate() {
                out.push(block.into_chain_block(from + offset as u64)?);
            }
        }
        if out.is_empty() {
            return Err(DaemonError::Request(format!(
                "get_blocks.bin returned no blocks from height {start_height}"
            )));
        }
        Ok(out)
    }

    /// `get_block_headers_range`: about a kilobyte a block. monerod refuses
    /// a range that runs past its tip, so a refused range is asked again as
    /// its first header alone: the caller gets fewer than it asked for, as
    /// from `get_chain_blocks`.
    async fn get_chain_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainHeader>, DaemonError> {
        #[derive(Deserialize)]
        struct Headers {
            #[serde(default)]
            headers: Vec<BlockHeader>,
        }
        if count == 0 {
            return Ok(Vec::new());
        }
        let count = count.min(MAX_HEADERS_PER_REQUEST);
        if count > 1 {
            let range = json!({
                "start_height": start_height,
                "end_height": start_height.saturating_add(count - 1),
            });
            if let Ok(Headers { headers }) =
                self.post_json_rpc("get_block_headers_range", range).await
            {
                let mut out = Vec::with_capacity(headers.len());
                for (height, header) in (start_height..).zip(headers) {
                    if header.height.is_some_and(|own| own != height) {
                        return Err(DaemonError::Request(format!(
                            "get_block_headers_range: asked for block {height}, the node sent another"
                        )));
                    }
                    out.push(header.into_chain_header(height));
                }
                if !out.is_empty() {
                    return Ok(out);
                }
            }
        }
        let header = self.block_header(start_height).await?;
        if header.height.is_some_and(|own| own != start_height) {
            return Err(DaemonError::Request(format!(
                "get_block_header_by_height: asked for block {start_height}, the node sent another"
            )));
        }
        Ok(vec![header.into_chain_header(start_height)])
    }

    async fn get_block_timestamp(&self, height: u64) -> Result<u64, DaemonError> {
        Ok(self.block_header(height).await?.timestamp)
    }

    async fn get_mempool_transactions(&self) -> Result<Vec<Transaction>, DaemonError> {
        let resp: GetTransactionPoolResponse =
            self.post_plain("/get_transaction_pool", json!({})).await?;
        Ok(decode_pool_best_effort(&resp.transactions))
    }

    /// The pool's transaction ids, followed by its changes where the node
    /// can say them: one small `get_blocks.bin` answer naming what entered
    /// and left since the last poll (with the new transactions' bodies,
    /// pruned), instead of the whole list every time. Every
    /// `POOL_RESYNC_INTERVAL` the node's plain list replaces what was
    /// followed, so a missed change doesn't last. A node that can't say
    /// changes is asked for the plain list each time, as before.
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        let mut pool = self.pool.lock().await;
        let now = Instant::now();
        if pool.follows_changes == Some(false)
            && pool.retry_changes_at.is_some_and(|at| now >= at)
        {
            pool.follows_changes = None;
        }
        if pool.follows_changes == Some(false) {
            return self.pool_hashes().await;
        }
        if pool.follows_changes == Some(true) {
            if pool
                .polled_at
                .is_some_and(|at| now.saturating_duration_since(at) < POOL_REUSE)
            {
                return Ok(pool.list());
            }
            if pool
                .resynced_at
                .is_none_or(|at| now.saturating_duration_since(at) >= POOL_RESYNC_INTERVAL)
            {
                let txids = self.pool_hashes().await?;
                pool.resync(txids);
                pool.resynced_at = Some(now);
                pool.polled_at = Some(now);
                return Ok(pool.list());
            }
        }
        let response = self
            .post(
                POOL_CHANGES_ENDPOINT,
                "/get_blocks.bin",
                pool_changes_request(pool.since),
                false,
            )
            .await?;
        // An answer that isn't a description of the pool (or isn't epee at
        // all) is a node that doesn't know the request.
        match parse_pool_changes(&response).ok().flatten() {
            Some(changes) => {
                if changes.full {
                    // A whole pool is as good as a resync.
                    pool.resynced_at = Some(now);
                }
                pool.apply(changes);
                pool.follows_changes = Some(true);
                pool.polled_at = Some(now);
                Ok(pool.list())
            }
            None => {
                if pool.follows_changes == Some(true) {
                    tracing::warn!(
                        node = %self.base_url,
                        "the node stopped answering requests for mempool changes - asking for the whole list instead"
                    );
                }
                *pool = PoolView {
                    follows_changes: Some(false),
                    retry_changes_at: Some(now + POOL_CHANGES_RETRY),
                    ..PoolView::default()
                };
                self.pool_hashes().await
            }
        }
    }

    async fn get_transactions(&self, txids: &[String]) -> Result<Vec<Transaction>, DaemonError> {
        // In batches, so one request never carries an unbounded list.
        let mut out = Vec::with_capacity(txids.len());
        for batch in txids.chunks(100) {
            out.extend(self.fetch_transactions(batch).await?);
        }
        Ok(out)
    }

    /// Pruned, each with the id the node names it by (checked against the
    /// body: `TxEntry::fetched`). Bodies that arrived with the pool's
    /// changes are handed over without asking. A transaction the node no
    /// longer has is left out; one that can't be decoded is left out and
    /// logged, since it can't be matched against a wallet however often it
    /// is fetched.
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        let mut out = Vec::with_capacity(txids.len());
        let mut missing = Vec::new();
        {
            let mut pool = self.pool.lock().await;
            for txid in txids {
                match pool.bodies.remove(txid) {
                    Some(tx) => out.push(FetchedTx {
                        txid: txid.clone(),
                        tx,
                    }),
                    None => missing.push(txid.clone()),
                }
            }
        }
        // In batches, so one request never carries an unbounded list.
        for batch in missing.chunks(TXS_PER_REQUEST) {
            let wanted: HashSet<&String> = batch.iter().collect();
            let resp = self.request_transactions(batch, true).await?;
            for entry in resp.txs.unwrap_or_default() {
                match entry.fetched() {
                    Ok(fetched) if wanted.contains(&fetched.txid) => out.push(fetched),
                    Ok(fetched) => {
                        return Err(DaemonError::Request(format!(
                            "daemon sent transaction {}, which was not asked for",
                            fetched.txid
                        )))
                    }
                    Err(e) => shared::throttled!(
                        format!("undecodable-tx:{}", self.base_url),
                        warn,
                        error = %e,
                        "skipping a transaction that can't be read - it can't be matched against any wallet either way"
                    ),
                }
            }
        }
        Ok(out)
    }

    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        let resp = self
            .request_transactions(std::slice::from_ref(&txid.to_string()), true)
            .await?;
        classify_located_transaction(txid, resp)
    }

    /// One `/get_transactions` (pruned: only the locations are read) for up
    /// to `TXS_PER_REQUEST` transactions at a time. Only an affirmative
    /// answer is given for a transaction: the node named it as missed
    /// (`NotFound`) or sent an entry that places it. Anything else is left
    /// out, for `locate_transaction` to ask about (and refuse a non-answer).
    async fn locate_transactions(
        &self,
        txids: &[String],
    ) -> Result<HashMap<String, TxLocation>, DaemonError> {
        let mut out = HashMap::with_capacity(txids.len());
        for batch in txids.chunks(TXS_PER_REQUEST) {
            let wanted: HashSet<&str> = batch.iter().map(String::as_str).collect();
            let resp = self.request_transactions(batch, true).await?;
            for missed in &resp.missed_tx {
                if let Some(txid) = wanted.get(missed.as_str()) {
                    out.insert(txid.to_string(), TxLocation::NotFound);
                }
            }
            for entry in resp.txs.unwrap_or_default() {
                let (Some(txid), Some(location)) =
                    (wanted.get(entry.tx_hash.as_str()), entry.location())
                else {
                    continue;
                };
                // Named both as missed and as found: no answer at all.
                if out.insert(txid.to_string(), location).is_some() {
                    out.remove(*txid);
                }
            }
        }
        Ok(out)
    }

    /// One `/get_transactions` for both the transaction (pruned) and where
    /// it is.
    async fn find_transaction(
        &self,
        txid: &str,
    ) -> Result<Option<(FetchedTx, TxLocation)>, DaemonError> {
        let resp = self
            .request_transactions(std::slice::from_ref(&txid.to_string()), true)
            .await?;
        let fetched = resp
            .txs
            .as_ref()
            .and_then(|txs| txs.first())
            .map(TxEntry::fetched)
            .transpose();
        let location = classify_located_transaction(txid, resp)?;
        if location == TxLocation::NotFound {
            return Ok(None);
        }
        let fetched = fetched?.ok_or_else(|| {
            DaemonError::Request(format!("daemon placed {txid} but sent no transaction"))
        })?;
        if fetched.txid != txid {
            return Err(DaemonError::Request(format!(
                "asked for transaction {txid}, the node sent {}",
                fetched.txid
            )));
        }
        Ok(Some((fetched, location)))
    }

    async fn get_transaction(&self, txid: &str) -> Result<Transaction, DaemonError> {
        // Reuses `fetch_transactions` - already exactly this shape
        // (`get_block_transactions` above already calls it with a block's own
        // hash list), just with a single-element list here.
        let mut txs = self
            .fetch_transactions(std::slice::from_ref(&txid.to_string()))
            .await?;
        txs.pop()
            .ok_or_else(|| DaemonError::Request(format!("no such transaction: {txid}")))
    }

    async fn is_key_image_spent(
        &self,
        key_images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        if key_images.is_empty() {
            return Ok(vec![]);
        }
        let resp: IsKeyImageSpentResponse = self
            .post_plain("/is_key_image_spent", json!({ "key_images": key_images }))
            .await?;
        // A caller correlates this response with `key_images` positionally (e.g. by
        // `zip`ping the two, as the scanner's double-spend check and
        // `tests/support/mod.rs`'s spendable-output filter both do) - a short
        // response would otherwise silently drop the tail of the request rather than
        // erroring, misreporting "unspent"/"not found" for whatever got dropped.
        if resp.spent_status.len() != key_images.len() {
            return Err(DaemonError::Request(format!(
                "daemon returned {} statuses for {} requested key images",
                resp.spent_status.len(),
                key_images.len()
            )));
        }
        Ok(resp
            .spent_status
            .into_iter()
            .map(|code| match code {
                1 => KeyImageStatus::SpentInBlockchain,
                2 => KeyImageStatus::SpentInPool,
                _ => KeyImageStatus::Unspent, // 0, or any unrecognized code - degrade to the safe (non-voiding) default
            })
            .collect())
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    //! Hermetic counterparts to the `#[ignore]`d live tests below: the response
    //! shapes a real node can produce, exercised without a network.

    use super::*;

    const FIXTURE_TX_HEX: &str = include_str!("../tests/fixtures/subaddress_tx.hex");

    /// A real mainnet block with only its coinbase (from the `monero` crate's
    /// own serialisation test).
    const COINBASE_ONLY_BLOCK_HEX: &str = "0c0c94debaf805beb3489c722a285c092a32e7c6893abfc7d069699c8326fc3445a749c5276b6200000000029b892201ffdf882201b699d4c8b1ec020223df524af2a2ef5f870adb6e1ceb03a475c39f8b9ef76aa50b46ddd2a18349402b012839bfa19b7524ec7488917714c216ca254b38ed0424ca65ae828a7c006aeaf10208f5316a7f6b99cca60000";

    /// A block's identity comes from its own blob: the id the node reports
    /// for it, its parent's id and its timestamp. A blob that doesn't match
    /// the transactions sent with it, or is missing, is an error, never a
    /// block with made-up identity.
    #[test]
    fn a_get_blocks_bin_entry_carries_its_own_block_identity() {
        let blob = hex::decode(COINBASE_ONLY_BLOCK_HEX).unwrap();
        let decoded: monero::Block = deserialize(&blob).unwrap();
        // The coinbase names the block's height.
        let height = match &decoded.miner_tx.prefix.inputs[0] {
            monero::blockdata::transaction::TxIn::Gen { height } => height.0,
            _ => unreachable!("a coinbase input"),
        };
        let block = BinBlock {
            block: Some(blob.clone()),
            txs: vec![],
        }
        .into_chain_block(height)
        .unwrap();
        assert_eq!(block.height, height);
        // A node answering from another height is refused, not recorded as
        // the block asked for.
        let error = BinBlock {
            block: Some(blob.clone()),
            txs: vec![],
        }
        .into_chain_block(height + 1)
        .unwrap_err();
        assert!(error.to_string().contains("the node sent block"), "{error}");
        assert_eq!(block.hash, hex::encode(decoded.id().0));
        assert_eq!(
            block.prev_hash,
            "beb3489c722a285c092a32e7c6893abfc7d069699c8326fc3445a749c5276b62"
        );
        assert_eq!(block.timestamp, decoded.header.timestamp.0);
        assert!(block.txs.is_empty());

        let extra = BinBlock {
            block: Some(blob),
            txs: vec![BinTx {
                blob: hex::decode(FIXTURE_TX_HEX.trim()).unwrap(),
                prunable_hash: None,
            }],
        }
        .into_chain_block(height);
        assert!(extra.is_err(), "a transaction the block doesn't list");
        assert!(BinBlock {
            block: None,
            txs: vec![]
        }
        .into_chain_block(height)
        .is_err());
        assert!(BinBlock {
            block: Some(vec![1, 2, 3]),
            txs: vec![]
        }
        .into_chain_block(height)
        .is_err());
    }

    fn entry(as_hex: &str) -> TxEntry {
        TxEntry {
            as_hex: as_hex.to_string(),
            in_pool: false,
            block_height: Some(1),
            ..Default::default()
        }
    }

    /// `get_info` from a current monerod (`nettype`), from an older one
    /// (only the flags), from a regtest node, and from one that says
    /// nothing about its network.
    #[test]
    fn get_info_says_which_network_a_node_is_on() {
        let nettype = |value: Value| {
            serde_json::from_value::<GetInfoResult>(value)
                .unwrap()
                .nettype()
        };
        assert_eq!(
            nettype(json!({ "nettype": "stagenet", "height": 5, "status": "OK" })),
            "stagenet"
        );
        assert_eq!(nettype(json!({ "nettype": "Mainnet" })), "mainnet");
        assert_eq!(
            nettype(json!({ "mainnet": false, "stagenet": false, "testnet": true })),
            "testnet"
        );
        assert_eq!(nettype(json!({ "nettype": "fakechain" })), "fakechain");
        assert_eq!(nettype(json!({ "height": 5 })), "unknown");
        let info = |nettype: &str| DaemonInfo {
            nettype: nettype.to_string(),
            height: None,
        };
        assert_eq!(info("testnet").network(), Some(monero::Network::Testnet));
        assert_eq!(
            info("fakechain").network(),
            None,
            "a regtest node is never on the wrong network"
        );
        assert_eq!(DaemonInfo::unknown().network(), None);
    }

    #[test]
    fn an_empty_mempool_response_omitting_the_transactions_key_entirely_parses_as_empty_not_an_error(
    ) {
        // Reported live against a real public testnet node: an empty pool came back
        // as `{"status":"OK",...}` with no "transactions" key at all, rather than
        // `"transactions": []` - and without `#[serde(default)]` that's a hard parse
        // error on *every* poll of a genuinely empty mempool, not a one-off. This
        // pins the fix directly against the response shape that broke, without
        // needing a live node or an HTTP mock.
        let resp: GetTransactionPoolResponse =
            serde_json::from_str(r#"{"status":"OK","untrusted":false}"#).unwrap();
        assert!(resp.transactions.is_empty());
        assert!(decode_pool_best_effort(&resp.transactions).is_empty());

        // The ordinary shape - an explicit empty array - still works too.
        let resp: GetTransactionPoolResponse =
            serde_json::from_str(r#"{"status":"OK","transactions":[]}"#).unwrap();
        assert!(resp.transactions.is_empty());

        // And a real entry still deserializes correctly alongside the fix.
        let resp: GetTransactionPoolResponse = serde_json::from_str(&format!(
            r#"{{"status":"OK","transactions":[{{"tx_blob":"{FIXTURE_TX_HEX}"}}]}}"#
        ))
        .unwrap();
        assert_eq!(decode_pool_best_effort(&resp.transactions).len(), 1);
    }

    #[test]
    fn a_block_whose_transactions_the_node_cannot_supply_is_an_error_not_a_short_list() {
        // The failure this exists to prevent: the scanner marks the block scanned
        // and moves on, so a payment in the dropped transaction is never seen and
        // nothing ever logs that anything was missing.
        let hashes = vec!["aa".repeat(32), "bb".repeat(32)];
        let resp = GetTransactionsResponse {
            txs: Some(vec![entry(FIXTURE_TX_HEX)]),
            missed_tx: vec!["bb".repeat(32)],
        };
        let err = decode_all_or_fail(&hashes, resp).unwrap_err();
        assert!(err.to_string().contains("could not supply"), "got {err}");
    }

    #[test]
    fn a_response_shorter_than_the_request_is_an_error_even_with_an_empty_missed_tx() {
        // Same loss of a payment, arrived at without the node admitting anything is
        // missing - so the `missed_tx` check alone isn't sufficient.
        let hashes = vec!["aa".repeat(32), "bb".repeat(32)];
        let resp = GetTransactionsResponse {
            txs: Some(vec![entry(FIXTURE_TX_HEX)]),
            missed_tx: vec![],
        };
        let err = decode_all_or_fail(&hashes, resp).unwrap_err();
        assert!(
            err.to_string().contains("for 2 requested hashes"),
            "got {err}"
        );

        let resp = GetTransactionsResponse {
            txs: None,
            missed_tx: vec![],
        };
        assert!(decode_all_or_fail(&hashes, resp).is_err());
    }

    #[test]
    fn a_complete_response_decodes_every_transaction() {
        let hashes = vec!["aa".repeat(32), "bb".repeat(32)];
        let resp = GetTransactionsResponse {
            txs: Some(vec![entry(FIXTURE_TX_HEX), entry(FIXTURE_TX_HEX)]),
            missed_tx: vec![],
        };
        let txs = decode_all_or_fail(&hashes, resp).unwrap();
        assert_eq!(txs.len(), 2);
        assert!(!txs[0].prefix.outputs.is_empty());
    }

    #[test]
    fn an_empty_request_needs_no_round_trip_and_yields_nothing() {
        assert!(decode_all_or_fail(
            &[],
            GetTransactionsResponse {
                txs: None,
                missed_tx: vec![]
            }
        )
        .unwrap()
        .is_empty());
    }

    #[test]
    fn a_pruned_transaction_with_no_blob_is_an_error_rather_than_a_silent_skip() {
        let err = decode_tx_hex("").unwrap_err();
        assert!(err.to_string().contains("no as_hex data"), "got {err}");
        assert!(decode_tx_hex("not hex at all").is_err());
        assert!(
            decode_tx_hex("deadbeef").is_err(),
            "valid hex that isn't a transaction"
        );
    }

    #[test]
    fn only_an_affirmative_miss_locates_a_transaction_as_not_found() {
        // `NotFound` is the one value that leads to a payment being voided and an
        // order stamped with a double-spend, and `/is_key_image_spent` returns no
        // spending txid, so nothing else in the system cross-checks that conclusion.
        // Every non-answer therefore has to be an error rather than a "no".
        let txid = "aa".repeat(32);

        // The daemon affirmatively says it doesn't have it. This is the only shape
        // that may report NotFound.
        assert_eq!(
            classify_located_transaction(
                &txid,
                GetTransactionsResponse {
                    txs: None,
                    missed_tx: vec![txid.clone()]
                }
            )
            .unwrap(),
            TxLocation::NotFound
        );

        // Ordinary positive answers are unaffected.
        assert_eq!(
            classify_located_transaction(
                &txid,
                GetTransactionsResponse {
                    txs: Some(vec![TxEntry {
                        as_hex: String::new(),
                        in_pool: false,
                        block_height: Some(3_755_690),
                        ..Default::default()
                    }]),
                    missed_tx: vec![],
                }
            )
            .unwrap(),
            TxLocation::InBlock(3_755_690)
        );
        assert_eq!(
            classify_located_transaction(
                &txid,
                GetTransactionsResponse {
                    txs: Some(vec![TxEntry {
                        as_hex: String::new(),
                        in_pool: true,
                        block_height: None,
                        ..Default::default()
                    }]),
                    missed_tx: vec![],
                }
            )
            .unwrap(),
            TxLocation::InPool
        );

        // A response that places the transaction nowhere and misses nothing. Every
        // field of this struct is optional out of necessity, so a bare
        // `{"status":"OK"}` from a load balancer, cache, or backend with nothing to
        // say deserializes into exactly this - and used to be read as "the
        // transaction is gone from the chain", which for a *confirmed* payment means
        // its own key images come back SpentInBlockchain and it gets voided as a
        // double-spend.
        let err = classify_located_transaction(
            &txid,
            GetTransactionsResponse {
                txs: None,
                missed_tx: vec![],
            },
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("neither a transaction nor a miss"),
            "got {err}"
        );
        assert!(
            serde_json::from_value::<GetTransactionsResponse>(json!({ "status": "OK" })).is_ok(),
            "the empty-but-valid response this guards against must really parse - otherwise the guard is moot"
        );

        // ...and a miss reported for some *other* hash is equally not an answer about
        // this one.
        assert!(classify_located_transaction(
            &txid,
            GetTransactionsResponse {
                txs: None,
                missed_tx: vec!["bb".repeat(32)]
            },
        )
        .is_err());

        // "Confirmed, but I won't say where" is self-contradictory, not a miss.
        let err = classify_located_transaction(
            &txid,
            GetTransactionsResponse {
                txs: Some(vec![TxEntry {
                    as_hex: String::new(),
                    in_pool: false,
                    block_height: None,
                    ..Default::default()
                }]),
                missed_tx: vec![],
            },
        )
        .unwrap_err();
        assert!(err.to_string().contains("no block_height"), "got {err}");
    }

    #[test]
    fn one_undecodable_mempool_entry_is_skipped_rather_than_blinding_the_whole_poll() {
        // The failure this prevents: `.map(..).collect::<Result<Vec<_>, _>>()` turned
        // a single unparseable pool entry into an error for the entire call, and
        // `run_scan_tick` treats a failed mempool poll as "no mempool this tick". So
        // one transaction this build can't deserialize - a future transaction format,
        // or anything at all that a stranger chose to broadcast - switches off
        // zero-conf detection for every tenant on that network for as long as it sits
        // in the pool. A public mempool holds transactions for days, so this is a
        // state anyone can put the scanner into, not a hypothetical.
        //
        // The opposite policy for blocks (`decode_all_or_fail`) is deliberate and
        // still right: a block is marked scanned exactly once, so dropping one of its
        // transactions loses a payment permanently. Nothing about the mempool is ever
        // marked done, and an undecodable transaction can never be matched anyway.
        let entries = vec![
            PoolTx {
                tx_blob: FIXTURE_TX_HEX.to_string(),
            },
            PoolTx {
                tx_blob: "not hex at all".to_string(),
            },
            PoolTx {
                tx_blob: String::new(),
            }, // pruned: present, no blob
            PoolTx {
                tx_blob: "deadbeef".to_string(),
            }, // valid hex, not a transaction
            PoolTx {
                tx_blob: FIXTURE_TX_HEX.to_string(),
            },
        ];
        let decoded = decode_pool_best_effort(&entries);
        assert_eq!(
            decoded.len(),
            2,
            "every decodable transaction must survive its undecodable neighbours"
        );
        for tx in &decoded {
            assert!(!tx.prefix.outputs.is_empty());
        }

        // A pool of nothing but junk is an empty mempool, not an error - there is
        // genuinely nothing to scan, and reporting that as a failed poll would be
        // indistinguishable from an unreachable node.
        assert!(decode_pool_best_effort(&[PoolTx {
            tx_blob: "zz".into()
        }])
        .is_empty());
        assert!(decode_pool_best_effort(&[]).is_empty());
    }

    #[test]
    fn get_height_response_parses_and_the_block_count_to_tip_height_adjustment_holds() {
        // monerod's `/get_height` reports the chain *length*; the trait contract is
        // the tip block's own *height*, one less. Pinned here (rather than only in
        // the `#[ignore]`d live test) so a future edit to that line has to
        // deliberately change an assertion.
        let parsed: GetHeightResponse = serde_json::from_value(json!({
            "height": 3_755_691u64, "hash": "ab", "status": "OK", "untrusted": false
        }))
        .unwrap();
        assert_eq!(parsed.height.saturating_sub(1), 3_755_690);
        // A single-block chain (genesis only) is height 0, not an underflow.
        assert_eq!(1u64.saturating_sub(1), 0);
        assert_eq!(0u64.saturating_sub(1), 0);
    }

    #[test]
    fn optional_fields_a_real_node_may_omit_still_parse() {
        // `tx_hashes` is absent on an empty block, and `missed_tx` is absent when
        // nothing was missed - a hard `Vec` on either would turn a normal response
        // into a parse error and stall the scanner.
        let block: GetBlockResult = serde_json::from_value(
            json!({ "block_header": { "hash": "abc", "timestamp": 1_700_000_000u64 } }),
        )
        .unwrap();
        assert_eq!(block.block_header.hash, "abc");
        assert_eq!(block.block_header.timestamp, 1_700_000_000);
        assert!(block.tx_hashes.is_empty());

        let txs: GetTransactionsResponse =
            serde_json::from_value(json!({ "status": "OK" })).unwrap();
        assert!(txs.txs.is_none() && txs.missed_tx.is_empty());

        // A pool entry reports no block height at all.
        let pool: TxEntry = serde_json::from_value(
            json!({ "as_hex": "ab", "in_pool": true, "block_height": null }),
        )
        .unwrap();
        assert!(pool.in_pool && pool.block_height.is_none());
    }

    #[test]
    fn an_in_pool_entry_with_no_block_height_key_at_all_deserializes_as_none() {
        // Distinct from the `block_height: null` case above, and the one that
        // actually matters: per `COMMAND_RPC_GET_TRANSACTIONS::entry`'s own
        // `KV_SERIALIZE_MAP` in monero-project/monero's source, `block_height` sits
        // inside `if (!this_ref.in_pool) { KV_SERIALIZE(block_height) ... }` - a
        // transaction still in the mempool never has this *key* in the response at
        // all, not a key present with a JSON `null`. Verifies serde's documented
        // behavior for `Option<T>` fields (defaults to `None` when the key is
        // missing, with no `#[serde(default)]` needed) actually holds here, rather
        // than trusting that behavior from memory.
        let entry: TxEntry =
            serde_json::from_value(json!({ "as_hex": "ab", "in_pool": true })).unwrap();
        assert!(entry.in_pool);
        assert!(entry.block_height.is_none());
    }

    #[test]
    fn spent_status_accepts_the_real_signed_int_type_and_an_absent_key() {
        // The real field is `std::vector<int> spent_status` - a signed 32-bit C++
        // int - not `u8`. A value outside 0-255 (or negative) used to fail
        // deserialization outright, before `is_key_image_spent`'s own `_ =>
        // Unspent` catch-all - meant to degrade unrecognized codes safely - ever
        // got a chance to run. monerod is documented to only ever send 0/1/2 today,
        // so this specific overflow was never observed live (unlike the two bugs
        // above, both found from a real error message) - this is a type-fidelity
        // fix caught by checking the source directly, not a reported failure.
        let resp: IsKeyImageSpentResponse =
            serde_json::from_value(json!({ "spent_status": [0, 1, 2, -1, 300] })).unwrap();
        assert_eq!(resp.spent_status, vec![0, 1, 2, -1, 300]);

        // And, consistent with every other vector field in this file, an absent
        // key parses as empty rather than a hard error.
        let resp: IsKeyImageSpentResponse =
            serde_json::from_value(json!({ "status": "OK" })).unwrap();
        assert!(resp.spent_status.is_empty());
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod live_node_tests {
    //! These hit a real public Monero node over the network and are excluded from
    //! the default `cargo test` run (`#[ignore]`) so the main suite stays hermetic
    //! and fast - run explicitly with `cargo test --ignored daemon_rpc::` when you
    //! want to verify against real chain data. Per `docs/TESTING.md` §10's
    //! reasoning for the regtest tier: this proves the real RPC wiring and real
    //! transaction parsing against real (and constantly-changing) mainnet data,
    //! which no fixture-based unit test can substitute for - it's a *complement* to
    //! the mocked-daemon scanner tests in `scanner.rs`, not a replacement.

    use super::*;

    const TEST_NODE_HOST: &str = "node.hollingworth.xyz";
    const TEST_NODE_PORT: u16 = 18089;

    fn client() -> RpcDaemonClient {
        RpcDaemonClient::new(TEST_NODE_HOST, TEST_NODE_PORT, true, true).unwrap()
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_height_returns_a_plausible_value() {
        let height = client().get_height().await.unwrap();
        // Mainnet passed height 3,700,000 in mid-2026; a sane lower bound that
        // won't need updating for a long time, without hardcoding an exact value
        // that would go stale on every run.
        assert!(
            height > 3_700_000,
            "height {height} looks implausible for current mainnet"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_block_hash_matches_a_known_immutable_block() {
        // Captured live against this exact node while building this client - at
        // 5+ confirmations deep at the time, this block's hash is permanent.
        let hash = client().get_block_hash(3_755_690).await.unwrap();
        assert_eq!(
            hash,
            "61dcf348728fd124895e5e9e5188cc34a13c483f84ddfb5d3998f38d0ae55aa4"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_block_timestamp_matches_a_known_immutable_block() {
        // Same block as `real_node_get_block_hash_matches_a_known_immutable_block`
        // above - captured live against this exact node while building this
        // client.
        let timestamp = client().get_block_timestamp(3_755_690).await.unwrap();
        assert_eq!(timestamp, 1_788_593_344);
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_find_height_at_or_before_matches_the_known_block() {
        // The default trait method (`daemon.rs`), exercised here against the
        // real RPC-backed `get_height`/`get_block_timestamp` rather than the
        // fake - proves the binary search itself, not just its two
        // primitives, works against the real node's actual (not perfectly
        // monotonic) timestamps.
        let height = client()
            .find_height_at_or_before(1_788_593_344)
            .await
            .unwrap();
        assert_eq!(height, 3_755_690);
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_block_transactions_all_parse_as_valid_monero_transactions() {
        // Proves the real deserialization path handles *current* mainnet
        // transaction formats (view tags, CLSAG, bulletproofs+) - the crate's own
        // fixture used elsewhere in this codebase is a single, older-format
        // transaction and wouldn't catch a version-compatibility regression here.
        // 87 non-coinbase transactions, captured live against this exact block -
        // `tx_hashes` (what this call fetches) already excludes the coinbase/miner
        // transaction, so this is the full regular-transaction count, not "minus
        // one" as an earlier version of this test wrongly assumed from
        // `block_header.num_txes` before actually running it against real data.
        let txs = client().get_block_transactions(3_755_690).await.unwrap();
        assert_eq!(txs.len(), 87);
        for tx in &txs {
            assert!(!tx.prefix.inputs.is_empty());
            assert!(!tx.prefix.outputs.is_empty());
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_blocks_range_matches_get_block_transactions_for_the_same_range() {
        // The one test that actually exercises `get_blocks.bin` against a real
        // node - everything else about this override (`daemon_rpc.rs`) was
        // written from a real, published reference implementation of the same
        // endpoint, not verified live; this is that verification. Compares a
        // real multi-block batched fetch against the same range fetched the
        // old, already-proven way (`get_block_transactions`, one call per
        // height) - same node, same blocks, transaction-for-transaction. Starts
        // one block before the known block both other live tests already use,
        // so this also covers the ordinary (non-genesis) `start_height` path
        // without needing its own separately-verified fixture block.
        let c = client();
        let start = 3_755_689;
        let count = 3;

        let batched = c.get_blocks_range(start, count).await.unwrap();
        assert_eq!(
            batched.len() as u64,
            count,
            "a real node should honor a small max_block_count"
        );

        use monero::cryptonote::hash::Hashable;
        for (offset, block_txs) in batched.iter().enumerate() {
            let height = start + offset as u64;
            let individually = c.get_block_transactions(height).await.unwrap();
            assert_eq!(
                block_txs.iter().map(Hashable::hash).collect::<Vec<_>>(),
                individually.iter().map(Hashable::hash).collect::<Vec<_>>(),
                "get_blocks_range's block {height} didn't match get_block_transactions for the same height"
            );
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_blocks_range_handles_the_start_height_zero_special_case() {
        // `get_blocks.bin` ignores `start_height: 0` (see `get_blocks_range`'s
        // own doc comment) - proves the genesis special-case actually reaches
        // real block 0 and real block 1 correctly, not just heights the bin
        // endpoint itself handles natively.
        let c = client();
        let blocks = c.get_blocks_range(0, 2).await.unwrap();
        assert_eq!(blocks.len(), 2);
        // The genesis block has no regular (non-coinbase) transactions on any
        // real Monero network.
        assert!(blocks[0].is_empty());
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_mempool_transactions_decode_successfully() {
        // The mempool is constantly changing, so this can't assert exact content -
        // only that whatever is currently there decodes cleanly, proving the same
        // path the live scanner would run every ~second in production.
        let txs = client().get_mempool_transactions().await.unwrap();
        for tx in &txs {
            assert!(!tx.prefix.outputs.is_empty());
        }
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_locate_transaction_finds_a_known_confirmed_tx() {
        let location = client()
            .locate_transaction("24f70768d285ca14dd8080a9cddf1ecdebce7553933c9a638090b5fda2101fa8")
            .await
            .unwrap();
        assert_eq!(location, TxLocation::InBlock(3_755_690));
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_transaction_matches_the_same_tx_fetched_via_its_block() {
        // `docs/txid_lookup_and_scan_chunking_wbs.md` Part B's own new
        // capability - proves it against the same known-confirmed txid
        // `real_node_locate_transaction_finds_a_known_confirmed_tx` already
        // uses, comparing the standalone fetch to that transaction's own copy
        // inside the already-proven `get_block_transactions` path, hash for
        // hash.
        let txid = "24f70768d285ca14dd8080a9cddf1ecdebce7553933c9a638090b5fda2101fa8";
        let c = client();
        let fetched = c.get_transaction(txid).await.unwrap();

        use monero::cryptonote::hash::Hashable;
        let block_txs = c.get_block_transactions(3_755_690).await.unwrap();
        let expected = block_txs
            .into_iter()
            .find(|tx| hex::encode(tx.hash().to_bytes()) == txid)
            .expect("the known txid must be one of this block's own transactions");
        assert_eq!(fetched.hash(), expected.hash());
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_get_transaction_errors_for_a_bogus_hash() {
        let result = client()
            .get_transaction("0000000000000000000000000000000000000000000000000000000000000000")
            .await;
        assert!(
            result.is_err(),
            "a nonexistent txid must be a real error, not a silently empty/default transaction"
        );
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_locate_transaction_reports_not_found_for_a_bogus_hash() {
        let location = client()
            .locate_transaction("0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .unwrap();
        assert_eq!(location, TxLocation::NotFound);
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_is_key_image_spent_reports_unspent_for_a_null_image() {
        let statuses = client()
            .is_key_image_spent(&[
                "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            ])
            .await
            .unwrap();
        assert_eq!(statuses, vec![KeyImageStatus::Unspent]);
    }

    #[tokio::test]
    #[ignore]
    async fn real_node_end_to_end_scan_of_live_mempool_never_panics_and_finds_no_false_matches() {
        // The fullest available proof this pipeline works: real transactions,
        // fresh off a real node's real mempool, run through the actual
        // scanner::scan_transaction_for_tenant path (real KeyCustody scan +
        // real Store) against a wallet that has never received anything. Expect
        // zero matches (this key owns nothing) - the point is that scanning
        // diverse, unpredictable real-world transaction shapes never errors or
        // panics, which a single canned fixture can't prove.
        use crate::key_custody::{KeyCustody, PlainKeyCustody, WalletMaterial};
        use crate::store::Store;
        use monero::{PrivateKey, PublicKey};

        let mut view_bytes = [0x42u8; 32];
        view_bytes[31] &= 0x0f;
        let view_key = PrivateKey::from_slice(&view_bytes).unwrap();
        let mut spend_bytes = [0x24u8; 32];
        spend_bytes[31] &= 0x0f;
        let spend_key = PrivateKey::from_slice(&spend_bytes).unwrap();
        let spend_pubkey = PublicKey::from_private_key(&spend_key);

        let key_custody = PlainKeyCustody::default();
        let handle = key_custody
            .register_wallet(WalletMaterial::new(
                view_key.to_bytes(),
                spend_pubkey.to_bytes(),
            ))
            .await
            .unwrap();
        let store = Store::open_in_memory().unwrap();
        let created = store
            .create_tenant(
                crate::store::NewTenant {
                    key_custody_backend: "plain".into(),
                    sealed_key_material: vec![],
                    primary_address: "4test".into(),
                    network: "mainnet".into(),
                    confirmations_required: None,
                    order_expiry_seconds: None,
                },
                0,
            )
            .unwrap();

        let txs = client().get_mempool_transactions().await.unwrap();
        assert!(
            !txs.is_empty(),
            "test needs a non-empty live mempool to be meaningful"
        );

        for tx in &txs {
            let touched = crate::scanner::scan_transaction_for_tenant(
                &store,
                &key_custody,
                handle,
                &created.tenant.id,
                tx,
                0..1,
                0,
                None,
            )
            .await
            .unwrap();
            assert!(touched.is_empty());
        }
    }

    #[tokio::test]
    async fn a_response_larger_than_the_cap_is_refused_as_a_node_error() {
        use axum::routing::post;
        // Any endpoint: a valid-looking answer padded to 10kB.
        let app = axum::Router::new().fallback(post(|| async {
            format!(
                "{{\"status\":\"OK\",\"height\":1,\"count\":1,\"pad\":\"{}\"}}",
                "x".repeat(10_000)
            )
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let small = RpcDaemonClient::new("127.0.0.1", port, false, false)
            .unwrap()
            .with_max_response_bytes(1_000);
        let err = small.get_height().await.unwrap_err();
        assert!(err.to_string().contains("larger than"), "got: {err}");

        let roomy = RpcDaemonClient::new("127.0.0.1", port, false, false).unwrap();
        assert!(
            roomy.get_height().await.is_ok(),
            "the same response is fine under the default cap"
        );
    }
}
