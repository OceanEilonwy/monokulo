//! Real `MoneroDaemonClient` implementation, talking to `monerod`'s actual RPC
//! surface over `reqwest` + `rustls`.
//!
//! Every endpoint and field name used here was verified against a live public
//! node (`node.hollingworth.xyz:18089`, restricted RPC) before being written, the
//! same way `key_custody`'s crypto was verified against `monero-rs`'s own source
//! rather than assumed from memory - see the `#[ignore]`d tests at the bottom of
//! this file for the live proof.
//!
//! Three RPC surfaces are in play: the JSON-RPC envelope at `/json_rpc` (block
//! hashes and headers, `get_info`), monerod's "other" plain-JSON endpoints
//! (`/get_height`, `/get_transaction_pool_hashes`, `/get_transactions`,
//! `/is_key_image_spent`) which are not wrapped in a `jsonrpc`/`result`
//! envelope at all, and the binary (epee) `/get_blocks.bin`, for blocks and
//! for changes to the mempool.
//!
//! What is asked for is kept small (`docs/node_rpc_efficiency.md)`:
//! transactions come pruned (the prefix and `RingCT` base a scan reads, about a
//! sixth of the bytes), a block's id comes from the block itself, a lone hash
//! or header is asked for as just that, and the mempool is followed by its
//! changes rather than re-listed. [`RpcDaemonClient::stats`] counts every
//! request and its bytes, by endpoint.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use monero::consensus::encode::deserialize;
use monero::cryptonote::hash::Hashable as _;
use monero::Transaction;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::time::Instant;

use crate::daemon::{
    ChainBlock, ChainHeader, ChainTip, DaemonError, DaemonInfo, DifficultyHeader, EndpointStats,
    FetchedTx, KeyImageStatus, MoneroDaemonClient, PoolAnswer, PoolOutlook, TxLocation,
};

pub struct RpcDaemonClient {
    client: reqwest::Client,
    base_url: String,
    /// Largest response body accepted (task 7.6). A node that sends more is
    /// treated as failing rather than being allowed to exhaust memory.
    max_response_bytes: usize,
    stats: parking_lot::Mutex<HashMap<String, EndpointStats>>,
    /// What has been measured of this node's link: what sizes and times
    /// block requests (`docs/engine_scaling.md` sections 1 and 2).
    link: crate::link::Link,
    /// The mempool as this node last described it. An async lock: one poll
    /// at a time, held across its round trip, so two loops polling at once
    /// can't apply the same changes out of order.
    pool: tokio::sync::Mutex<PoolView>,
    /// `POOL_REUSE` and `POOL_RESYNC_INTERVAL`; tests shorten them.
    pool_reuse: Duration,
    pool_resync: Duration,
    /// Set when the node announced a pool change since the last poll
    /// (`pool_changed`): the next poll asks, whatever `pool_reuse` says.
    pool_stale: std::sync::atomic::AtomicBool,
    /// The tip as this node last gave it with its id. A poll of the pool
    /// that names this block learns in the same answer whether the chain
    /// still ends there (`get_tip_and_mempool`).
    tip: parking_lot::Mutex<Option<ChainTip>>,
}

/// Default cap on one response body: comfortably above a full mempool under
/// load or a `get_blocks.bin` chunk sized by `payment.scan_chunk_memory_budget_mb`,
/// far below what would hurt the machine.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// How long one request to a node may take.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// A `reqwest` failure as a [`DaemonError`]: a timeout says so, since a
/// smaller request may succeed where this one ran out of time.
fn request_error(context: &str, e: &reqwest::Error) -> DaemonError {
    if e.is_timeout() {
        DaemonError::TimedOut(format!("{context}: {e}"))
    } else {
        DaemonError::Request(format!("{context}: {e}"))
    }
}

/// Bodies at most this size count as round-trip samples.
const SMALL_RESPONSE_BYTES: usize = 16 * 1024;

/// How one request's time divided: until its first byte, and from there to
/// its last.
struct Timing {
    to_first_byte: Duration,
    transfer: Duration,
}

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
        return Err(DaemonError::TooLarge(format!(
            "response from {what} is larger than the {cap}-byte limit"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| request_error(&format!("reading response from {what}"), &e))?
    {
        if body.len() + chunk.len() > cap {
            return Err(DaemonError::TooLarge(format!(
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
    /// **on** for the reason above, with `monero_node.strict_tls` as the
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
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|e| DaemonError::Request(format!("failed to build HTTP client: {e}")))?;
        // An IPv6 literal goes in brackets; anything else that isn't a
        // host name or address makes no URL.
        let host = if host.contains(':') && !host.starts_with('[') {
            format!("[{host}]")
        } else {
            host.to_owned()
        };
        let base_url = format!("{scheme}://{host}:{port}");
        url::Url::parse(&base_url)
            .ok()
            .filter(|url| url.host_str().is_some())
            .ok_or_else(|| DaemonError::Request(format!("not a node address: {host}:{port}")))?;
        Ok(Self {
            client,
            base_url,
            max_response_bytes: MAX_RESPONSE_BYTES,
            stats: parking_lot::Mutex::default(),
            link: crate::link::Link::default(),
            pool: tokio::sync::Mutex::default(),
            pool_reuse: POOL_REUSE,
            pool_resync: POOL_RESYNC_INTERVAL,
            pool_stale: std::sync::atomic::AtomicBool::default(),
            tip: parking_lot::Mutex::default(),
        })
    }

    /// Other pool timings, for tests: how long an answer is reused, and how
    /// often the plain list of ids replaces what was followed.
    #[doc(hidden)]
    #[must_use = "the client with the timing set is returned, not changed in place"]
    pub fn with_pool_timing(mut self, reuse: Duration, resync: Duration) -> Self {
        self.pool_reuse = reuse;
        self.pool_resync = resync;
        self
    }

    /// Forgets when the pool was last asked about, so the next poll asks
    /// the node whatever the reuse window: for tests that would otherwise
    /// wait it out.
    #[doc(hidden)]
    pub async fn forget_pool_answer_time(&self) {
        self.pool.lock().await.polled_at = None;
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
    /// counting both under `endpoint`. A small answer is a round-trip
    /// sample for the link.
    async fn post(
        &self,
        endpoint: &str,
        path: &str,
        body: Vec<u8>,
        json: bool,
    ) -> Result<Vec<u8>, DaemonError> {
        let started = Instant::now();
        let bytes = self
            .post_timed(
                endpoint,
                path,
                body,
                json,
                REQUEST_TIMEOUT,
                self.max_response_bytes,
            )
            .await?
            .0;
        if bytes.len() <= SMALL_RESPONSE_BYTES {
            self.link.record_small(started.elapsed());
        }
        Ok(bytes)
    }

    /// [`Self::post`] within `timeout`, saying how the time divided. A
    /// timeout and any other failure are noted against the link.
    async fn post_timed(
        &self,
        endpoint: &str,
        path: &str,
        body: Vec<u8>,
        json: bool,
        timeout: Duration,
        cap: usize,
    ) -> Result<(Vec<u8>, Timing), DaemonError> {
        let result = self
            .post_timed_inner(endpoint, path, body, json, timeout, cap)
            .await;
        match &result {
            Err(DaemonError::TimedOut(_)) => self.link.record_timeout(),
            Err(_) => self.link.record_failure(),
            Ok(_) => {}
        }
        result
    }

    async fn post_timed_inner(
        &self,
        endpoint: &str,
        path: &str,
        body: Vec<u8>,
        json: bool,
        timeout: Duration,
        cap: usize,
    ) -> Result<(Vec<u8>, Timing), DaemonError> {
        let started = Instant::now();
        let sent = body.len() as u64;
        {
            let mut stats = self.stats.lock();
            let entry = stats
                .entry(endpoint.to_owned())
                .or_insert_with(|| EndpointStats {
                    endpoint: endpoint.to_owned(),
                    ..Default::default()
                });
            entry.requests += 1;
            entry.bytes_sent += sent;
        }
        let mut request = self
            .client
            .post(format!("{}{path}", self.base_url))
            .timeout(timeout);
        if json {
            request = request.header(reqwest::header::CONTENT_TYPE, "application/json");
        }
        let response = request
            .body(body)
            .send()
            .await
            .map_err(|e| request_error(&format!("{endpoint} after {timeout:?}"), &e))?;
        let first_byte = Instant::now();
        let bytes = read_capped(response, cap, endpoint).await?;
        if let Some(entry) = self.stats.lock().get_mut(endpoint) {
            entry.bytes_received += bytes.len() as u64;
        }
        let timing = Timing {
            to_first_byte: first_byte.duration_since(started),
            transfer: first_byte.elapsed(),
        };
        Ok((bytes, timing))
    }

    /// A request for the engine page alone: counted under `endpoint` like
    /// any other, but never a round-trip sample or a failure for the link
    /// the scan sizes its requests from, so watching the page can't change
    /// how the engine scans.
    async fn post_unsampled(
        &self,
        endpoint: &str,
        path: &str,
        body: &Value,
    ) -> Result<Value, DaemonError> {
        let (bytes, _) = self
            .post_timed_inner(
                endpoint,
                path,
                body.to_string().into_bytes(),
                true,
                REQUEST_TIMEOUT,
                self.max_response_bytes,
            )
            .await?;
        serde_json::from_slice(&bytes)
            .map_err(|e| DaemonError::Request(format!("invalid JSON response from {path}: {e}")))
    }

    /// Lowers the response size cap, for tests.
    #[cfg(test)]
    fn with_max_response_bytes(mut self, max_response_bytes: usize) -> Self {
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
        json_rpc_result(method, &bytes)
    }

    /// [`Self::post_json_rpc`] within `timeout`, accepting an answer of up
    /// to `cap` bytes: for an answer whose size the caller knows to expect.
    async fn post_json_rpc_within<T: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cap: usize,
    ) -> Result<T, DaemonError> {
        let body = json!({ "jsonrpc": "2.0", "id": "0", "method": method, "params": params });
        let (bytes, _) = self
            .post_timed(
                method,
                "/json_rpc",
                body.to_string().into_bytes(),
                true,
                timeout,
                cap,
            )
            .await?;
        json_rpc_result(method, &bytes)
    }
}

/// The `result` of a JSON-RPC answer, or the error the node gave.
fn json_rpc_result<T: for<'de> Deserialize<'de>>(
    method: &str,
    bytes: &[u8],
) -> Result<T, DaemonError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|e| DaemonError::Request(format!("invalid JSON response: {e}")))?;
    if let Some(err) = value.get("error") {
        return Err(DaemonError::Request(format!(
            "daemon RPC error calling {method}: {err}"
        )));
    }
    let result = value
        .get("result")
        .ok_or_else(|| DaemonError::Request(format!("missing 'result' field calling {method}")))?;
    serde_json::from_value(result.clone())
        .map_err(|e| DaemonError::Request(format!("failed to parse result of {method}: {e}")))
}

impl RpcDaemonClient {
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
        let full = value.to_string();
        serde_json::from_value(value).map_err(|e| {
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

    /// `/get_transactions` for `hashes`, pruned: each one's prefix and
    /// `RingCT` base (what a scan reads) with the hash of the rest.
    async fn request_transactions(
        &self,
        hashes: &[String],
    ) -> Result<GetTransactionsResponse, DaemonError> {
        let body = json!({ "txs_hashes": hashes, "decode_as_json": false, "prune": true });
        self.post_plain("/get_transactions", body).await
    }

    /// `get_blocks.bin` for blocks, always with `start_height >= 1`: monerod
    /// only observes the field when it is non-zero (`get_chain_blocks`
    /// reads the genesis block from its header).
    async fn get_blocks_bin(
        &self,
        start_height: u64,
        max_block_count: u64,
    ) -> Result<Vec<BinBlock>, DaemonError> {
        let request = get_blocks_bin_request(start_height, max_block_count);
        let timeout = self.link.timeout_for_blocks(max_block_count);
        let (response, timing) = self
            .post_timed(
                "/get_blocks.bin",
                "/get_blocks.bin",
                request,
                false,
                timeout,
                self.blocks_cap(max_block_count),
            )
            .await?;
        let blocks = parse_get_blocks_bin_response(&response)?;
        let received = response.len();
        // The blocks' blobs are copied out: the answer they came in goes now,
        // not when the whole chunk is decoded (docs/engine_scaling.md 3).
        drop(response);
        self.link.record_blocks(
            blocks.len() as u64,
            received,
            timing.to_first_byte,
            timing.transfer,
        );
        Ok(blocks)
    }

    /// The largest `get_blocks.bin` answer accepted for `blocks` blocks:
    /// four times what this link's blocks have averaged, and never under
    /// the general cap. A larger answer is refused as too large, and the
    /// scan asks for fewer blocks next time.
    fn blocks_cap(&self, blocks: u64) -> usize {
        let expected = blocks as f64 * self.link.bytes_per_block();
        ((expected * 4.0).min(usize::MAX as f64) as usize).max(self.max_response_bytes)
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

/// Turns a single-hash `/get_transactions` response into a [`TxLocation`], insisting
/// that the daemon actually answered the question before reporting `NotFound`.
///
/// `NotFound` is not an ordinary "no" here - it is the single most consequential
/// value this whole client can return. `engine::check_for_reorg_and_reconcile` reacts
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
/// `engine::tests::a_reconciliation_that_fails_partway_leaves_the_reorg_still_detectable`
/// pins. Trading a retry for never voiding a live payment off a response that never
/// said it was gone is not a close call.
fn classify_located_transaction(
    txid: &str,
    resp: GetTransactionsResponse,
) -> Result<TxLocation, DaemonError> {
    if resp.missed_tx.iter().any(|h| h == txid) {
        return Ok(TxLocation::NotFound);
    }
    // The entry for the transaction asked about, not whatever came first:
    // an entry for another hash (a cache or proxy answering for someone
    // else) is a non-answer.
    let entry = resp.txs.and_then(|v| {
        v.into_iter()
            .find(|entry| entry.tx_hash.is_empty() || entry.tx_hash.eq_ignore_ascii_case(txid))
    });
    match entry {
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

/// One field of an epee request object.
enum EpeeField<'a> {
    Bool(bool),
    U8(u8),
    U64(u64),
    /// A byte string of under 64 bytes (one block id).
    Blob(&'a [u8]),
}

/// An epee-encoded request: a flat object of `fields`, in order.
///
/// The encoding: an 8-byte magic header, a 1-byte version, then the object
/// as a compact-varint field count followed by `(1-byte name length, name
/// bytes, 1-byte type tag, value)` per field - see `monero_epee`'s own
/// module docs for the full format. A count under 64 fits the varint's
/// 1-byte form (`count << 2`), which is all this ever needs.
#[expect(
    clippy::expect_used,
    reason = "only called with a few short field-name literals"
)]
fn epee_request(fields: &[(&str, EpeeField<'_>)]) -> Vec<u8> {
    let mut request = Vec::with_capacity(96);
    request.extend_from_slice(&monero_epee::HEADER);
    request.push(monero_epee::VERSION);
    let count = u8::try_from(fields.len()).expect("more request fields than fit a u8");
    assert!(
        count < 64,
        "more request fields than the 1-byte varint holds"
    );
    request.push(count << 2);
    for (name, value) in fields {
        request.push(u8::try_from(name.len()).expect("field name literal longer than 255 bytes"));
        request.extend_from_slice(name.as_bytes());
        #[expect(
            clippy::as_conversions,
            reason = "epee type tags are the enum discriminants, each below 256"
        )]
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
            EpeeField::Blob(bytes) => {
                request.push(monero_epee::Type::String as u8);
                let len = u8::try_from(bytes.len()).expect("a blob longer than 255 bytes");
                assert!(len < 64, "a blob longer than the 1-byte varint holds");
                request.push(len << 2);
                request.extend_from_slice(bytes);
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
/// Pruned: each transaction comes as its prefix and `RingCT` base (what a
/// scan reads) with the hash of the rest, from which its id is checked
/// against the block's own list (`BinBlock::into_chain_block`).
fn get_blocks_bin_request(start_height: u64, max_block_count: u64) -> Vec<u8> {
    epee_request(&[
        ("prune", EpeeField::Bool(true)),
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
/// the added transactions whole: a pool entry carries no prunable hash, so
/// only a whole body can be checked against the id it came under (see
/// `PoolView::apply`). The request wallets poll with.
fn pool_changes_request(since: u64) -> Vec<u8> {
    epee_request(&[
        ("requested_info", EpeeField::U8(REQUESTED_INFO_POOL_ONLY)),
        ("pool_info_since", EpeeField::U64(since)),
        ("prune", EpeeField::Bool(false)),
    ])
}

/// `get_blocks.bin`'s `requested_info` for "blocks and the pool".
const REQUESTED_INFO_BLOCKS_AND_POOL: u8 = 1;

/// [`pool_changes_request`], asking in the same request whether the chain
/// still ends at the block `tip_id`. monerod answers a `block_ids` that
/// starts with its own top block with no blocks at all, only the chain's
/// length and the pool's changes: nothing new. Otherwise it sends blocks
/// from `start_height`: one, from the start of the chain (a few hundred
/// bytes, of no interest in itself), which says the tip has moved.
fn pool_changes_and_tip_request(since: u64, tip_id: &[u8; 32]) -> Vec<u8> {
    epee_request(&[
        (
            "requested_info",
            EpeeField::U8(REQUESTED_INFO_BLOCKS_AND_POOL),
        ),
        ("pool_info_since", EpeeField::U64(since)),
        ("prune", EpeeField::Bool(false)),
        ("block_ids", EpeeField::Blob(tip_id)),
        ("start_height", EpeeField::U64(1)),
        ("max_block_count", EpeeField::U64(1)),
        ("no_miner_tx", EpeeField::Bool(true)),
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
        let wire_bytes =
            (blob.len() + self.txs.iter().map(|entry| entry.blob.len()).sum::<usize>()) as u64;
        let mut txs = Vec::with_capacity(self.txs.len());
        let mut txids = Vec::with_capacity(self.txs.len());
        for (entry, listed) in self.txs.iter().zip(&block.tx_hashes) {
            let txid = hex::encode(listed.0);
            let tx = decode_tx_blob(&entry.blob, entry.prunable_hash.as_ref(), Some(&txid))
                .map_err(|e| {
                    DaemonError::Request(format!("get_blocks.bin: block {height}: {e}"))
                })?;
            // Only what the scan reads is kept; the transaction itself is
            // dropped here.
            txs.push(crate::daemon::ScanTx::of(&tx.tx));
            txids.push(txid);
        }
        Ok(ChainBlock {
            height,
            hash: hex::encode(block.id().0),
            prev_hash: hex::encode(block.header.prev_id.0),
            timestamp: block.header.timestamp.0,
            txs,
            txids,
            wire_bytes,
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
/// prunable hash sent - a node does not send one for every transaction;
/// stagenet nodes answer `get_blocks.bin` without) is the claimed id taken
/// as given: in a block, that is the block's own list, which its id covers.
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
        (None, Some(claimed)) if is_txid(claimed) => claimed.to_owned(),
        (None, _) => return Err("a pruned transaction came without an id".to_owned()),
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
                                                "get_blocks.bin: a tx entry (object form) had no blob field".to_owned(),
                                            )
                                        })?,
                                        prunable_hash,
                                    }
                                }
                                other @ (monero_epee::Type::Int64
                                | monero_epee::Type::Int32
                                | monero_epee::Type::Int16
                                | monero_epee::Type::Int8
                                | monero_epee::Type::Uint64
                                | monero_epee::Type::Uint32
                                | monero_epee::Type::Uint16
                                | monero_epee::Type::Uint8
                                | monero_epee::Type::Double
                                | monero_epee::Type::Bool) => {
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
        Some(s) if s == b"OK" => {}
        Some(s) => {
            return Err(DaemonError::Request(format!(
                "get_blocks.bin returned status {:?}",
                String::from_utf8_lossy(&s)
            )));
        }
        None => {
            return Err(DaemonError::Request(
                "get_blocks.bin response had no status field".to_owned(),
            ))
        }
    }

    blocks.ok_or_else(|| {
        DaemonError::Request("get_blocks.bin response had no blocks field".to_owned())
    })
}

/// What a node said changed in its pool (`get_blocks.bin` with
/// `requested_info` = the pool, with or without blocks).
#[derive(Debug, Default, PartialEq)]
struct PoolChanges {
    /// Blocks that came with the answer, and the chain's length (0 when
    /// the answer doesn't say): what [`pool_changes_and_tip_request`]
    /// reads the tip from.
    blocks: usize,
    chain_length: u64,
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

/// Parses a `get_blocks.bin` response about the pool. `Ok(None)` when the node
/// answered without describing its pool: one that doesn't know the request
/// (an older monerod), to be asked the old way instead.
fn parse_pool_changes(bytes: &[u8]) -> Result<Option<PoolChanges>, DaemonError> {
    fn ids(blob: &[u8]) -> Result<Vec<String>, DaemonError> {
        let (whole, rest) = blob.as_chunks::<32>();
        if !rest.is_empty() {
            return Err(DaemonError::Request(format!(
                "invalid pool get_blocks.bin response: a transaction id list of {} bytes",
                blob.len()
            )));
        }
        Ok(whole.iter().map(hex::encode).collect())
    }
    let epee_err = |e| epee_err("pool get_blocks.bin", e);

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
            b"current_height" => changes.chain_length = value.to_u64().map_err(epee_err)?,
            b"blocks" => {
                let mut entries = value.iterate().map_err(epee_err)?;
                while let Some(item) = entries.next() {
                    item.map_err(epee_err)?;
                    changes.blocks += 1;
                }
            }
            b"remaining_added_pool_txids" => {
                changes.added_ids = ids(value.to_str().map_err(epee_err)?.consume())?;
            }
            b"removed_pool_txids" => {
                changes.removed = ids(value.to_str().map_err(epee_err)?.consume())?;
            }
            b"added_pool_txs" => {
                let mut entries = value.iterate().map_err(epee_err)?;
                while let Some(item) = entries.next() {
                    let mut tx_fields = item.map_err(epee_err)?.fields().map_err(epee_err)?;
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
                                "pool get_blocks.bin: an added transaction had no id".to_owned(),
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
const POOL_CHANGES_RETRY: Duration = Duration::from_mins(10);
/// After this many requests for changes in a row that a node which used to
/// answer them didn't, it is asked the old way.
const POOL_CHANGES_GIVE_UP: u32 = 3;
/// Most blocks a node sends with the pool's changes when the tip has moved
/// (one is asked for; monerod may round up to three). More, and the node
/// doesn't read the request as meant: it is asked for the tip apart.
const TIP_MOVED_BLOCKS_MAX: usize = 3;
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
    /// Requests for changes in a row that got no description of the pool.
    unanswered: u32,
    /// Until when the tip isn't asked about with the pool's changes: the
    /// node didn't answer the two together as monerod does.
    tip_apart_until: Option<Instant>,
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
            // Only a body that hashes to the id it came under is kept: the
            // pool entry has no prunable hash, so a pruned body (a node
            // ignoring `prune = false`) can't be checked and is left to be
            // fetched by `/get_transactions`, which sends one. A body that
            // doesn't decode or hashes to something else is left to be
            // fetched (and reported) the same way.
            if self.bodies.len() < POOL_BODIES_MAX {
                match decode_tx_blob(&blob, None, Some(&txid)) {
                    Ok(fetched) if !shared::monero_tx::is_pruned(&fetched.tx) => {
                        self.bodies.insert(txid.clone(), fetched.tx);
                    }
                    Ok(_) => {}
                    Err(error) => shared::throttled!(
                        "pool-body-mismatch",
                        warn,
                        tx.id = %txid,
                        error = %error,
                        "the node sent a pool transaction that isn't the one it named"
                    ),
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
    /// Transactions in the pool.
    #[serde(default)]
    tx_pool_size: Option<u64>,
    /// The median weight of recent blocks, as the penalty is reckoned from.
    #[serde(default)]
    block_weight_median: Option<u64>,
}

/// `/get_transaction_pool_stats`: only the pool's totals are read.
#[derive(Deserialize)]
struct PoolStatsResponse {
    pool_stats: PoolStats,
}

#[derive(Deserialize)]
struct PoolStats {
    bytes_total: u64,
    txs_total: u64,
}

impl GetInfoResult {
    fn nettype(&self) -> String {
        match &self.nettype {
            Some(nettype) if !nettype.trim().is_empty() => nettype.trim().to_ascii_lowercase(),
            _ if self.mainnet => "mainnet".to_owned(),
            _ if self.stagenet => "stagenet".to_owned(),
            _ if self.testnet => "testnet".to_owned(),
            _ => DaemonInfo::UNKNOWN.to_owned(),
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
    #[serde(default)]
    block_weight: Option<u64>,
    #[serde(default)]
    num_txes: Option<u64>,
    /// The difficulty as two 64-bit halves, as monerod sends it; read only
    /// for an anchor's window (`docs/proof_of_work.md`).
    #[serde(default)]
    difficulty: u64,
    #[serde(default)]
    difficulty_top64: u64,
    #[serde(default)]
    cumulative_difficulty: u64,
    #[serde(default)]
    cumulative_difficulty_top64: u64,
}

impl BlockHeader {
    /// The header as the chain tier and block recorder use it. The genesis
    /// block's parent is the empty string here, not monerod's 64 zeros. The
    /// hashes are checked for shape and lowercased, as `get_block_hash`'s
    /// are: a hash recorded in another form would never equal the one read
    /// later, and read as a reorg at that height forever.
    fn into_chain_header(self, height: u64) -> Result<ChainHeader, DaemonError> {
        let hash = self.hash.to_ascii_lowercase();
        if !is_txid(&hash) {
            return Err(DaemonError::Request(format!(
                "block {height}'s header has an invalid hash: {:?}",
                self.hash
            )));
        }
        let prev_hash = if height == 0 {
            String::new()
        } else {
            let prev_hash = self.prev_hash.to_ascii_lowercase();
            if !is_txid(&prev_hash) {
                return Err(DaemonError::Request(format!(
                    "block {height}'s header has an invalid parent hash: {:?}",
                    self.prev_hash
                )));
            }
            prev_hash
        };
        Ok(ChainHeader {
            height,
            hash,
            prev_hash,
            timestamp: self.timestamp,
            weight: self.block_weight,
            tx_count: self.num_txes,
        })
    }
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
/// `pruned_as_hex` (its prefix and `RingCT` base) with `prunable_hash` (the hash
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
/// `#[serde(default)]` added for consistency with every other vector-typed
/// field in this file, even though the only caller never sends an empty request (so an empty,
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
pub(crate) const TXS_PER_REQUEST: usize = 100;
/// What one transaction adds to a `get_block` answer: its id in the blob, in
/// the blob's JSON and in the list, as hex text with quotes and commas.
const OUTLINE_BYTES_PER_TX: usize = 256;
/// Most headers asked for in one `get_block_headers_range` request (a
/// restricted node refuses more than 1000).
pub(crate) const MAX_HEADERS_PER_REQUEST: u64 = 500;
/// The names pool-change polls are counted under in
/// [`RpcDaemonClient::stats`]: the same path as block fetches, different
/// requests.
const POOL_CHANGES_ENDPOINT: &str = "/get_blocks.bin (pool changes)";
const POOL_CHANGES_AND_TIP_ENDPOINT: &str = "/get_blocks.bin (pool changes and tip)";

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

    /// One poll of the pool (see `get_mempool_txids`), returning its
    /// transaction ids. With `known_tip`, a request for the pool's changes
    /// also asks whether the chain still ends at that block
    /// ([`pool_changes_and_tip_request`]); the flag returned is `true`
    /// only when the node said it does.
    async fn poll_pool(
        &self,
        known_tip: Option<&ChainTip>,
    ) -> Result<(Vec<String>, bool), DaemonError> {
        let mut pool = self.pool.lock().await;
        let now = Instant::now();
        if self
            .pool_stale
            .swap(false, std::sync::atomic::Ordering::Relaxed)
        {
            pool.polled_at = None;
        }
        if pool.follows_changes == Some(false) && pool.retry_changes_at.is_some_and(|at| now >= at)
        {
            pool.follows_changes = None;
        }
        if pool.follows_changes == Some(false) {
            return Ok((self.pool_hashes().await?, false));
        }
        if pool.follows_changes == Some(true) {
            if pool
                .polled_at
                .is_some_and(|at| now.saturating_duration_since(at) < self.pool_reuse)
            {
                return Ok((pool.list(), false));
            }
            if pool
                .resynced_at
                .is_none_or(|at| now.saturating_duration_since(at) >= self.pool_resync)
            {
                let txids = self.pool_hashes().await?;
                pool.resync(txids);
                pool.resynced_at = Some(Instant::now());
                pool.polled_at = Some(Instant::now());
                return Ok((pool.list(), false));
            }
        }
        let mut tip_id = known_tip
            .and_then(|tip| tip.hash.as_deref())
            .and_then(|hash| hex::decode(hash).ok())
            .and_then(|bytes| hash32(&bytes))
            .filter(|_| pool.tip_apart_until.is_none_or(|until| now >= until));
        let parsed = loop {
            let (endpoint, request) = match &tip_id {
                Some(tip_id) => (
                    POOL_CHANGES_AND_TIP_ENDPOINT,
                    pool_changes_and_tip_request(pool.since, tip_id),
                ),
                None => (POOL_CHANGES_ENDPOINT, pool_changes_request(pool.since)),
            };
            let response = self
                .post(endpoint, "/get_blocks.bin", request, false)
                .await?;
            let parsed = parse_pool_changes(&response);
            if tip_id.is_some() && !matches!(parsed, Ok(Some(_))) {
                // Not an answer about the pool. The node may not take the
                // two questions together: for a while they are asked
                // apart, starting now.
                pool.tip_apart_until = Some(now + POOL_CHANGES_RETRY);
                tip_id = None;
                continue;
            }
            break parsed;
        };
        let changes = match parsed {
            Ok(Some(changes)) => changes,
            // An answer that isn't a description of the pool (or isn't epee
            // at all). From a node that has never given one, that is a node
            // that doesn't know the request: it is asked for the plain list
            // from now on. From a node that has, it is a failed answer (a
            // busy node, an error page from something in front of it),
            // unless it keeps happening.
            unanswered => {
                pool.unanswered += 1;
                if pool.follows_changes == Some(true) && pool.unanswered < POOL_CHANGES_GIVE_UP {
                    return Err(DaemonError::Request(match unanswered {
                        Err(error) => error.to_string(),
                        _ => "the node did not describe its mempool".to_owned(),
                    }));
                }
                if pool.follows_changes == Some(true) {
                    tracing::warn!(
                        node = %self.base_url,
                        "the node stopped answering requests for mempool changes - asking for the whole list instead"
                    );
                }
                *pool = PoolView {
                    follows_changes: Some(false),
                    retry_changes_at: Some(now + POOL_CHANGES_RETRY),
                    tip_apart_until: pool.tip_apart_until,
                    ..PoolView::default()
                };
                return Ok((self.pool_hashes().await?, false));
            }
        };
        // No blocks and the same length: the chain still ends at the block
        // the request named. Blocks: it doesn't.
        let tip_unchanged = match known_tip.filter(|_| tip_id.is_some()) {
            Some(known) => {
                let unchanged = changes.blocks == 0
                    && changes.chain_length.checked_sub(1) == Some(known.height);
                if !unchanged && !(1..=TIP_MOVED_BLOCKS_MAX).contains(&changes.blocks) {
                    pool.tip_apart_until = Some(now + POOL_CHANGES_RETRY);
                }
                unchanged
            }
            None => false,
        };
        if changes.full {
            // A whole pool is as good as a resync.
            pool.resynced_at = Some(Instant::now());
        }
        pool.apply(changes);
        pool.follows_changes = Some(true);
        pool.unanswered = 0;
        pool.polled_at = Some(Instant::now());
        Ok((pool.list(), tip_unchanged))
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for RpcDaemonClient {
    fn rpc_stats(&self) -> Vec<EndpointStats> {
        self.stats()
    }

    fn link(&self) -> Option<crate::link::LinkSnapshot> {
        Some(self.link.snapshot())
    }

    fn link_cost(&self) -> Option<crate::link::LinkCost> {
        Some(self.link.cost())
    }

    fn chain_blocks_timeout(&self, count: u64) -> Duration {
        self.link.timeout_for_blocks(count)
    }

    fn transfer_timeout(&self, bytes: u64) -> Duration {
        crate::link::timeout_for(self.link.expected_for_blocks(1, bytes as f64))
    }

    fn pool_changed(&self) {
        self.pool_stale
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

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
        // `get_block_hash`/`get_chain_blocks`. Originally misdiagnosed as a
        // load-balancer inconsistency (see the defensive one-block seed margin in
        // `engine::run_scan_tick`) before checking (1)-(3) above.
        Ok(resp.height.saturating_sub(1))
    }

    /// `/get_height` again, keeping the tip block's id it carries: the
    /// height and the id are read under one lock in monerod, so they name
    /// the same block. An id of the wrong shape is dropped, not trusted.
    async fn get_tip(&self) -> Result<ChainTip, DaemonError> {
        let resp: GetHeightResponse = self.post_plain("/get_height", json!({})).await?;
        let tip = ChainTip {
            height: resp.height.saturating_sub(1),
            hash: resp
                .hash
                .map(|hash| hash.to_ascii_lowercase())
                .filter(|hash| is_txid(hash) && resp.height > 0),
        };
        *self.tip.lock() = Some(tip.clone()).filter(|tip| tip.hash.is_some());
        Ok(tip)
    }

    /// One request for both while the chain still ends at the tip this
    /// node last gave: the poll for the pool's changes names that block,
    /// and monerod answers "nothing new" with the changes. When the tip
    /// has moved (or isn't known yet, or the pool was read some other way
    /// this time), the tip is asked for as usual.
    async fn get_tip_and_mempool(&self) -> (Result<ChainTip, DaemonError>, PoolAnswer) {
        let known = self.tip.lock().clone();
        match (self.poll_pool(known.as_ref()).await, known) {
            (Ok((txids, true)), Some(tip)) => (Ok(tip), Ok(txids)),
            (pool, _) => (self.get_tip().await, pool.map(|(txids, _)| txids)),
        }
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

    /// `get_info` for the pool's size and the median block weight, and
    /// `/get_transaction_pool_stats` for the pool's bytes: a node that
    /// refuses the second still gives the count.
    /// Neither request touches the link's estimates (`post_unsampled`).
    async fn get_pool_outlook(&self) -> Result<Option<PoolOutlook>, DaemonError> {
        let body = json!({ "jsonrpc": "2.0", "id": "0", "method": "get_info", "params": {} });
        let answer = self.post_unsampled("get_info", "/json_rpc", &body).await?;
        let info: GetInfoResult = json_rpc_result("get_info", answer.to_string().as_bytes())?;
        let stats = self
            .post_unsampled(
                "/get_transaction_pool_stats",
                "/get_transaction_pool_stats",
                &json!({}),
            )
            .await
            .ok()
            .filter(|value| value.get("status").and_then(Value::as_str) == Some("OK"))
            .and_then(|value| serde_json::from_value::<PoolStatsResponse>(value).ok());
        let Some(txs) = stats
            .as_ref()
            .map(|stats| stats.pool_stats.txs_total)
            .or(info.tx_pool_size)
        else {
            return Ok(None);
        };
        Ok(Some(PoolOutlook {
            txs,
            bytes: stats.map(|stats| stats.pool_stats.bytes_total),
            penalty_free: info
                .block_weight_median
                .unwrap_or(0)
                .max(PoolOutlook::FULL_REWARD_ZONE),
        }))
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

    /// `get_blocks.bin`, decoding each block's own header: one round trip
    /// for the range, and every block's id computed from the same blob its
    /// transactions came with. Genesis from its header: monerod only
    /// observes `start_height` when it is non-zero.
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let from = if start_height == 0 {
            // Its header names the genesis block and dates it. It holds no
            // transactions but its coinbase, on any network.
            let header = self.block_header(0).await?;
            out.push(ChainBlock {
                height: 0,
                hash: header.hash,
                prev_hash: String::new(),
                timestamp: header.timestamp,
                txs: Vec::new(),
                txids: Vec::new(),
                wire_bytes: 0,
            });
            1
        } else {
            start_height
        };
        let wanted = count - out.len() as u64;
        if wanted > 0 {
            let blocks = self.get_blocks_bin(from, wanted).await?;
            for (offset, block) in blocks.into_iter().take(wanted as usize).enumerate() {
                let block = block.into_chain_block(from + offset as u64)?;
                // One answer, one chain: a node mixing blocks from two
                // forks is refused here, not found by reorg detection
                // later.
                if let Some(previous) = out.last() {
                    if block.prev_hash != previous.hash {
                        return Err(DaemonError::Request(format!(
                            "get_blocks.bin: block {} does not follow block {} in the same answer",
                            block.height, previous.height
                        )));
                    }
                }
                out.push(block);
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
            match self.post_json_rpc("get_block_headers_range", range).await {
                Ok(Headers { headers }) => {
                    let mut out = Vec::with_capacity(headers.len());
                    for (height, header) in
                        (start_height..).zip(headers.into_iter().take(count as usize))
                    {
                        if header.height.is_some_and(|own| own != height) {
                            return Err(DaemonError::Request(format!(
                                "get_block_headers_range: asked for block {height}, the node sent another"
                            )));
                        }
                        out.push(header.into_chain_header(height)?);
                    }
                    if !out.is_empty() {
                        return Ok(out);
                    }
                }
                // Refused past the tip, which is expected; a node that
                // refuses every range makes this one header a call, which
                // an operator should be able to see.
                Err(error) => shared::throttled!(
                    "headers-range-refused",
                    debug,
                    error = %error,
                    "get_block_headers_range refused; asking for one header"
                ),
            }
        }
        let header = self.block_header(start_height).await?;
        if header.height.is_some_and(|own| own != start_height) {
            return Err(DaemonError::Request(format!(
                "get_block_header_by_height: asked for block {start_height}, the node sent another"
            )));
        }
        Ok(vec![header.into_chain_header(start_height)?])
    }

    /// `get_block` by height: the block's blob (its header, coinbase and
    /// transactions' ids) without the transactions, checked as a
    /// `get_blocks.bin` block is: the coinbase must name the height asked
    /// for, and the id is computed from the blob. The answer carries each
    /// id three times over (the blob, its JSON and the list), so it may be
    /// as large as `tx_count` needs, and gets the time the link needs.
    async fn get_block_outline(
        &self,
        height: u64,
        tx_count: Option<u64>,
    ) -> Result<crate::daemon::BlockOutline, DaemonError> {
        #[derive(Deserialize)]
        struct GetBlock {
            blob: String,
        }
        let expected = usize::try_from(tx_count.unwrap_or(0))
            .unwrap_or(usize::MAX)
            .saturating_mul(OUTLINE_BYTES_PER_TX);
        let cap = expected.saturating_add(self.max_response_bytes);
        let timeout = self.transfer_timeout(expected as u64);
        let answer: GetBlock = self
            .post_json_rpc_within("get_block", json!({ "height": height }), timeout, cap)
            .await?;
        let blob = hex::decode(answer.blob.trim()).map_err(|e| {
            DaemonError::Request(format!("get_block: block {height}'s blob isn't hex: {e}"))
        })?;
        drop(answer);
        let block: monero::Block = deserialize(&blob).map_err(|e| {
            DaemonError::Request(format!(
                "get_block: block {height} could not be decoded: {e}"
            ))
        })?;
        match block.miner_tx.prefix.inputs.first() {
            Some(monero::blockdata::transaction::TxIn::Gen { height: own }) if own.0 == height => {}
            _ => {
                return Err(DaemonError::Request(format!(
                    "get_block: asked for block {height}, the node sent another"
                )))
            }
        }
        Ok(crate::daemon::BlockOutline {
            height,
            hash: hex::encode(block.id().0),
            prev_hash: if height == 0 {
                String::new()
            } else {
                hex::encode(block.header.prev_id.0)
            },
            timestamp: block.header.timestamp.0,
            txids: block.tx_hashes.iter().map(|id| hex::encode(id.0)).collect(),
        })
    }

    /// `get_block` by height, keeping only the blob: about a kilobyte
    /// plus 32 bytes a transaction, sent hex-encoded with the same ids again
    /// as JSON (about 9 KB for 30 transactions). The coinbase's height is
    /// checked by the caller, which decodes it anyway.
    async fn get_block_blob(&self, height: u64) -> Result<Vec<u8>, DaemonError> {
        #[derive(Deserialize)]
        struct GetBlock {
            blob: String,
        }
        let answer: GetBlock = self
            .post_json_rpc("get_block", json!({ "height": height }))
            .await?;
        hex::decode(answer.blob.trim()).map_err(|e| {
            DaemonError::Request(format!("get_block: block {height}'s blob isn't hex: {e}"))
        })
    }

    /// `get_block_headers_range`, with each header's difficulty and
    /// cumulative difficulty.
    async fn get_difficulty_headers(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<DifficultyHeader>, DaemonError> {
        #[derive(Deserialize)]
        struct Headers {
            #[serde(default)]
            headers: Vec<BlockHeader>,
        }
        let count = count.clamp(1, MAX_HEADERS_PER_REQUEST);
        let range = json!({
            "start_height": start_height,
            "end_height": start_height.saturating_add(count - 1),
        });
        let Headers { headers } = self.post_json_rpc("get_block_headers_range", range).await?;
        let mut out = Vec::with_capacity(headers.len());
        for (height, header) in (start_height..).zip(headers.into_iter().take(count as usize)) {
            if header.height.is_some_and(|own| own != height) {
                return Err(DaemonError::Request(format!(
                    "get_block_headers_range: asked for block {height}, the node sent another"
                )));
            }
            let wide = |top: u64, low: u64| (u128::from(top) << 64) | u128::from(low);
            let difficulty = wide(header.difficulty_top64, header.difficulty);
            let cumulative_difficulty = wide(
                header.cumulative_difficulty_top64,
                header.cumulative_difficulty,
            );
            let chain = header.into_chain_header(height)?;
            out.push(DifficultyHeader {
                height,
                hash: chain.hash,
                prev_hash: chain.prev_hash,
                timestamp: chain.timestamp,
                difficulty,
                cumulative_difficulty,
            });
        }
        if out.is_empty() {
            return Err(DaemonError::Request(format!(
                "get_block_headers_range returned no headers from height {start_height}"
            )));
        }
        Ok(out)
    }

    /// The pool's transaction ids, followed by its changes where the node
    /// can say them: one small `get_blocks.bin` answer naming what entered
    /// and left since the last poll (with the new transactions' bodies,
    /// pruned), instead of the whole list every time. Every
    /// `POOL_RESYNC_INTERVAL` the node's plain list replaces what was
    /// followed, so a missed change doesn't last. A node that can't say
    /// changes is asked for the plain list each time, as before.
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        Ok(self.poll_pool(None).await?.0)
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
            let resp = self.request_transactions(batch).await?;
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
            .request_transactions(std::slice::from_ref(&txid.to_owned()))
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
            let resp = self.request_transactions(batch).await?;
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
            .request_transactions(std::slice::from_ref(&txid.to_owned()))
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
    pub(super) const COINBASE_ONLY_BLOCK_HEX: &str = "0c0c94debaf805beb3489c722a285c092a32e7c6893abfc7d069699c8326fc3445a749c5276b6200000000029b892201ffdf882201b699d4c8b1ec020223df524af2a2ef5f870adb6e1ceb03a475c39f8b9ef76aa50b46ddd2a18349402b012839bfa19b7524ec7488917714c216ca254b38ed0424ca65ae828a7c006aeaf10208f5316a7f6b99cca60000";

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
            monero::blockdata::transaction::TxIn::ToKey {
                amount: _,
                key_offsets: _,
                k_image: _,
            } => unreachable!("a coinbase input"),
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
        BinBlock {
            block: None,
            txs: vec![],
        }
        .into_chain_block(height)
        .unwrap_err();
        BinBlock {
            block: Some(vec![1, 2, 3]),
            txs: vec![],
        }
        .into_chain_block(height)
        .unwrap_err();
    }

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
            nettype: nettype.to_owned(),
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
        classify_located_transaction(
            &txid,
            GetTransactionsResponse {
                txs: None,
                missed_tx: vec!["bb".repeat(32)],
            },
        )
        .unwrap_err();

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
        // `missed_tx` is absent when nothing was missed - a hard `Vec` would
        // turn a normal response into a parse error and stall the scanner.
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
mod wire_tests {
    //! The requests that keep a node's work small, against a scripted node:
    //! the pool followed by its changes, several transactions asked about at
    //! once, pruned transactions checked against their ids, and the counts
    //! kept of all of it.

    use super::*;
    use monero::consensus::encode::serialize;
    use parking_lot::Mutex;
    use std::collections::VecDeque;
    use std::sync::Arc;

    const FIXTURE_TX_HEX: &str = include_str!("../tests/fixtures/subaddress_tx.hex");

    fn whole_tx() -> Transaction {
        deserialize(&hex::decode(FIXTURE_TX_HEX.trim()).unwrap()).unwrap()
    }

    /// The fixture transaction as a node sends it pruned: its id, its
    /// prefix and `RingCT` base, and the hash of what was left out.
    /// The fixture transaction whole, with its id.
    fn whole_fixture() -> (String, Vec<u8>) {
        let whole = whole_tx();
        (hex::encode(whole.hash().to_bytes()), serialize(&whole))
    }

    fn pruned_fixture() -> (String, Vec<u8>, [u8; 32]) {
        let whole = whole_tx();
        let base = whole.rct_signatures.sig.as_ref().unwrap();
        let mut blob = serialize(&whole.prefix);
        blob.extend(serialize(base));
        let mut prunable = std::io::Cursor::new(Vec::new());
        whole
            .rct_signatures
            .p
            .as_ref()
            .unwrap()
            .consensus_encode(&mut prunable, base.rct_type)
            .unwrap();
        (
            hex::encode(whole.hash().to_bytes()),
            blob,
            monero::Hash::new(prunable.into_inner()).to_bytes(),
        )
    }

    // -- A minimal epee writer, for scripted responses ----------------------

    enum V {
        Str(Vec<u8>),
        U8(u8),
        U64(u64),
        Objects(Vec<Vec<(&'static str, Self)>>),
    }

    fn varint(n: usize) -> Vec<u8> {
        if n < 64 {
            vec![(n as u8) << 2]
        } else if n < 0x4000 {
            (((n as u16) << 2) | 1).to_le_bytes().to_vec()
        } else {
            (((n as u32) << 2) | 2).to_le_bytes().to_vec()
        }
    }

    fn write_fields(fields: &[(&'static str, V)], out: &mut Vec<u8>) {
        out.extend(varint(fields.len()));
        for (name, value) in fields {
            out.push(name.len() as u8);
            out.extend_from_slice(name.as_bytes());
            match value {
                V::Str(bytes) => {
                    out.push(monero_epee::Type::String as u8);
                    out.extend(varint(bytes.len()));
                    out.extend_from_slice(bytes);
                }
                V::U8(value) => {
                    out.push(monero_epee::Type::Uint8 as u8);
                    out.push(*value);
                }
                V::U64(value) => {
                    out.push(monero_epee::Type::Uint64 as u8);
                    out.extend_from_slice(&value.to_le_bytes());
                }
                V::Objects(objects) => {
                    out.push(monero_epee::Type::Object as u8 | 0x80);
                    out.extend(varint(objects.len()));
                    for object in objects {
                        write_fields(object, out);
                    }
                }
            }
        }
    }

    fn epee(fields: &[(&'static str, V)]) -> Vec<u8> {
        let mut out = monero_epee::HEADER.to_vec();
        out.push(monero_epee::VERSION);
        write_fields(fields, &mut out);
        out
    }

    fn ids_blob(txids: &[&str]) -> Vec<u8> {
        txids
            .iter()
            .flat_map(|txid| hex::decode(txid).unwrap())
            .collect()
    }

    /// A pool answer: `extent` 2 for the whole pool, 1 for changes.
    fn pool_answer(
        extent: u8,
        daemon_time: u64,
        added: &[(&str, &[u8])],
        added_ids: &[&str],
        removed: &[&str],
    ) -> Vec<u8> {
        epee(&pool_fields(extent, daemon_time, added, added_ids, removed))
    }

    /// An answer to the pool's changes asked for with the tip: nothing
    /// changed in the pool, the chain is `chain_length` blocks long, and
    /// `blocks` blocks came with it (none: the tip named is still the tip).
    fn pool_and_chain_answer(daemon_time: u64, chain_length: u64, blocks: usize) -> Vec<u8> {
        let mut fields = pool_fields(POOL_INFO_INCREMENTAL, daemon_time, &[], &[], &[]);
        fields.push(("current_height", V::U64(chain_length)));
        if blocks > 0 {
            fields.push((
                "blocks",
                V::Objects(
                    std::iter::repeat_with(|| vec![("block", V::Str(vec![1, 2, 3]))])
                        .take(blocks)
                        .collect(),
                ),
            ));
        }
        epee(&fields)
    }

    fn pool_fields(
        extent: u8,
        daemon_time: u64,
        added: &[(&str, &[u8])],
        added_ids: &[&str],
        removed: &[&str],
    ) -> Vec<(&'static str, V)> {
        let mut fields = vec![
            ("status", V::Str(b"OK".to_vec())),
            ("pool_info_extent", V::U8(extent)),
            ("daemon_time", V::U64(daemon_time)),
        ];
        if !added.is_empty() {
            fields.push((
                "added_pool_txs",
                V::Objects(
                    added
                        .iter()
                        .map(|(txid, blob)| {
                            vec![
                                ("tx_hash", V::Str(hex::decode(txid).unwrap())),
                                ("tx_blob", V::Str(blob.to_vec())),
                                ("double_spend_seen", V::U8(0)),
                            ]
                        })
                        .collect(),
                ),
            ));
        }
        if !added_ids.is_empty() {
            fields.push(("remaining_added_pool_txids", V::Str(ids_blob(added_ids))));
        }
        if !removed.is_empty() {
            fields.push(("removed_pool_txids", V::Str(ids_blob(removed))));
        }
        fields
    }

    // -- A scripted node -----------------------------------------------------

    /// Answers each path from a queue (the last answer repeats), and keeps
    /// every request it got.
    #[derive(Default)]
    struct Script {
        answers: Mutex<HashMap<String, VecDeque<Vec<u8>>>>,
        requests: Mutex<Vec<(String, Vec<u8>)>>,
    }

    impl Script {
        fn answer(&self, path: &str, body: impl Into<Vec<u8>>) {
            self.answers
                .lock()
                .entry(path.to_owned())
                .or_default()
                .push_back(body.into());
        }

        fn requests_to(&self, path: &str) -> Vec<Vec<u8>> {
            self.requests
                .lock()
                .iter()
                .filter(|(p, _)| p == path)
                .map(|(_, body)| body.clone())
                .collect()
        }
    }

    async fn scripted() -> (RpcDaemonClient, Arc<Script>) {
        use axum::extract::State;
        let script = Arc::new(Script::default());
        let app = axum::Router::new()
            .fallback(
                async |State(script): State<Arc<Script>>,
                       uri: axum::http::Uri,
                       body: axum::body::Bytes| {
                    let path = uri.path().to_owned();
                    script.requests.lock().push((path.clone(), body.to_vec()));
                    let mut answers = script.answers.lock();
                    match answers.get_mut(&path) {
                        Some(queue) if queue.len() > 1 => {
                            (axum::http::StatusCode::OK, queue.pop_front().unwrap())
                        }
                        Some(queue) if !queue.is_empty() => {
                            (axum::http::StatusCode::OK, queue[0].clone())
                        }
                        _ => (
                            axum::http::StatusCode::NOT_FOUND,
                            b"<html>no such thing</html>".to_vec(),
                        ),
                    }
                },
            )
            .with_state(Arc::clone(&script));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RpcDaemonClient::new("127.0.0.1", port, false, false)
            .unwrap()
            // Every poll asks; the plain list is never due.
            .with_pool_timing(Duration::ZERO, Duration::from_secs(3600));
        (client, script)
    }

    fn sorted(mut txids: Vec<String>) -> Vec<String> {
        txids.sort();
        txids
    }

    /// The `pool_info_since` a pool request carried.
    fn since_of(request: &[u8]) -> u64 {
        let mut epee = monero_epee::Epee::new(request).unwrap();
        let mut fields = epee.entry().unwrap().fields().unwrap();
        while let Some(entry) = fields.next() {
            let (key, value) = entry.unwrap();
            if key.consume() == b"pool_info_since" {
                return value.to_u64().unwrap();
            }
        }
        panic!("no pool_info_since in the request");
    }

    /// The block a request for the pool's changes named as the tip, if it
    /// named one.
    fn tip_named(request: &[u8]) -> Option<String> {
        let mut epee = monero_epee::Epee::new(request).unwrap();
        let mut fields = epee.entry().unwrap().fields().unwrap();
        while let Some(entry) = fields.next() {
            let (key, value) = entry.unwrap();
            if key.consume() == b"block_ids" {
                return Some(hex::encode(value.to_str().unwrap().consume()));
            }
        }
        None
    }

    fn height_answer(chain_length: u64, tip_id: &str) -> String {
        json!({ "status": "OK", "height": chain_length, "hash": tip_id }).to_string()
    }

    fn tip(height: u64, id: &str) -> ChainTip {
        ChainTip {
            height,
            hash: Some(id.to_owned()),
        }
    }

    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
    const D: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

    /// The blocks request is, byte for byte, the one recorded against real
    /// nodes.
    #[test]
    fn the_blocks_request_is_the_recorded_one() {
        assert_eq!(
            hex::encode(get_blocks_bin_request(2_210_330, 4)),
            "0111010101010201010c057072756e650b010c73746172745f686569676874051aba2100000000000f6d61785f626c6f636b5f636f756e74050400000000000000"
        );
        assert_eq!(since_of(&pool_changes_request(77)), 77);
        // The request for the pool's changes and the tip, as recorded
        // against real nodes: the tip's id as a 32-byte string.
        let both = pool_changes_and_tip_request(77, &[0xab; 32]);
        assert_eq!(since_of(&both), 77);
        assert_eq!(tip_named(&both), Some("ab".repeat(32)));
        assert_eq!(
            hex::encode(&both),
            [
                "0111010101010201011c",
                "0e7265717565737465645f696e666f0801",
                "0f706f6f6c5f696e666f5f73696e6365054d00000000000000",
                "057072756e650b00",
                "09626c6f636b5f6964730a80",
                &"ab".repeat(32),
                "0c73746172745f686569676874050100000000000000",
                "0f6d61785f626c6f636b5f636f756e74050100000000000000",
                "0b6e6f5f6d696e65725f74780b01",
            ]
            .concat()
        );
    }

    /// The first poll gets the whole pool, with bodies; each later one asks
    /// only for what changed since the node's own clock at the last answer,
    /// and applies it. Bodies that came with an answer are handed over
    /// without a request, once.
    #[tokio::test]
    async fn the_pool_is_followed_by_its_changes() {
        let (client, node) = scripted().await;
        let (a, blob) = whole_fixture();
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_FULL, 100, &[(&a, &blob)], &[B], &[]),
        );
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_INCREMENTAL, 105, &[], &[C], &[B]),
        );
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_INCREMENTAL, 110, &[], &[], &[]),
        );

        assert_eq!(
            sorted(client.get_mempool_txids().await.unwrap()),
            sorted(vec![a.clone(), B.to_owned()])
        );
        // The body came with the pool: no request for it, and it is whole
        // (checked against its id) and carries the id.
        let bodies = client
            .get_transactions_with_ids(std::slice::from_ref(&a))
            .await
            .unwrap();
        assert_eq!(bodies.len(), 1);
        assert_eq!(bodies[0].txid, a);
        assert!(!shared::monero_tx::is_pruned(&bodies[0].tx));
        assert_eq!(bodies[0].tx.prefix, whole_tx().prefix);
        assert!(node.requests_to("/get_transactions").is_empty());

        assert_eq!(
            sorted(client.get_mempool_txids().await.unwrap()),
            sorted(vec![a.clone(), C.to_owned()])
        );
        assert_eq!(
            sorted(client.get_mempool_txids().await.unwrap()),
            sorted(vec![a.clone(), C.to_owned()]),
            "nothing changed"
        );
        let polls = node.requests_to("/get_blocks.bin");
        assert_eq!(
            polls.iter().map(|r| since_of(r)).collect::<Vec<_>>(),
            vec![0, 100, 105],
            "each poll asks for changes since the last answer's time"
        );
        assert!(
            node.requests_to("/get_transaction_pool_hashes").is_empty(),
            "the plain list is never asked for"
        );
        // All of it counted, under its own name.
        let stats = client.stats();
        let pool = stats
            .iter()
            .find(|s| s.endpoint == POOL_CHANGES_ENDPOINT)
            .unwrap();
        assert_eq!(pool.requests, 3);
        assert!(pool.bytes_sent > 0 && pool.bytes_received > 0);
        assert_eq!(MoneroDaemonClient::rpc_stats(&client), stats);
    }

    /// An answer reused within `POOL_REUSE` costs no request; a whole-pool
    /// answer replaces what was followed; and the plain list, when due,
    /// replaces it too (dropping a transaction whose removal was missed).
    #[tokio::test]
    async fn a_followed_pool_is_reused_briefly_and_corrected_by_the_plain_list() {
        let (client, node) = scripted().await;
        let client = client.with_pool_timing(Duration::from_secs(3600), Duration::ZERO);
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_FULL, 100, &[], &[B, C], &[]),
        );
        node.answer(
            "/get_transaction_pool_hashes",
            format!(r#"{{"status":"OK","tx_hashes":["{C}","{D}"]}}"#),
        );
        assert_eq!(client.get_mempool_txids().await.unwrap().len(), 2);
        assert_eq!(client.get_mempool_txids().await.unwrap().len(), 2);
        assert_eq!(node.requests_to("/get_blocks.bin").len(), 1, "reused");

        // Reuse over, and the plain list due: it is what the pool now is.
        let client = client.with_pool_timing(Duration::ZERO, Duration::ZERO);
        assert_eq!(
            sorted(client.get_mempool_txids().await.unwrap()),
            sorted(vec![C.to_owned(), D.to_owned()])
        );
        assert_eq!(node.requests_to("/get_transaction_pool_hashes").len(), 1);
        assert_eq!(node.requests_to("/get_blocks.bin").len(), 1);
    }

    /// A node that announced a pool change is asked again at once, however
    /// recent the last answer: it no longer describes the pool.
    #[tokio::test]
    async fn an_announced_pool_change_is_asked_about_within_the_reuse_window() {
        let (client, node) = scripted().await;
        let client = client.with_pool_timing(Duration::from_secs(3600), Duration::from_secs(3600));
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_FULL, 100, &[], &[B, C], &[]),
        );
        client.get_mempool_txids().await.unwrap();
        client.get_mempool_txids().await.unwrap();
        assert_eq!(node.requests_to("/get_blocks.bin").len(), 1, "reused");

        client.pool_changed();
        client.get_mempool_txids().await.unwrap();
        assert_eq!(node.requests_to("/get_blocks.bin").len(), 2, "asked again");
        client.get_mempool_txids().await.unwrap();
        assert_eq!(
            node.requests_to("/get_blocks.bin").len(),
            2,
            "once per announcement"
        );
    }

    /// A node that answers the pool request without describing its pool
    /// (an older monerod), or not in epee at all, is asked for the plain
    /// list instead, and not asked for changes again for a while.
    #[tokio::test]
    async fn a_node_that_cannot_say_pool_changes_is_asked_for_the_list() {
        for answer in [
            epee(&[("status", V::Str(b"OK".to_vec()))]),
            epee(&[("status", V::Str(b"Failed".to_vec()))]),
            b"<html>502 Bad Gateway</html>".to_vec(),
        ] {
            let (client, node) = scripted().await;
            node.answer("/get_blocks.bin", answer);
            node.answer(
                "/get_transaction_pool_hashes",
                format!(r#"{{"status":"OK","tx_hashes":["{B}"]}}"#),
            );
            for _ in 0..3 {
                assert_eq!(
                    client.get_mempool_txids().await.unwrap(),
                    vec![B.to_owned()]
                );
            }
            assert_eq!(node.requests_to("/get_blocks.bin").len(), 1);
            assert_eq!(node.requests_to("/get_transaction_pool_hashes").len(), 3);
        }
    }

    /// A node that has described its pool and then, once or twice, doesn't
    /// (a busy node, an error page) has failed those polls: what was
    /// followed is kept and the next poll carries on from it. Only if it
    /// keeps failing is it asked for the plain list instead.
    #[tokio::test]
    async fn a_node_that_stops_saying_pool_changes_fails_the_poll_before_it_is_given_up_on() {
        let (client, node) = scripted().await;
        let garbage = || b"<html>502 Bad Gateway</html>".to_vec();
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_FULL, 100, &[], &[B], &[]),
        );
        node.answer("/get_blocks.bin", garbage());
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_INCREMENTAL, 105, &[], &[C], &[]),
        );
        for _ in 0..POOL_CHANGES_GIVE_UP {
            node.answer("/get_blocks.bin", garbage());
        }
        node.answer(
            "/get_transaction_pool_hashes",
            format!(r#"{{"status":"OK","tx_hashes":["{D}"]}}"#),
        );
        assert_eq!(
            client.get_mempool_txids().await.unwrap(),
            vec![B.to_owned()]
        );
        // One bad answer: an error, and nothing forgotten.
        client.get_mempool_txids().await.unwrap_err();
        assert_eq!(
            sorted(client.get_mempool_txids().await.unwrap()),
            sorted(vec![B.to_owned(), C.to_owned()])
        );
        assert_eq!(since_of(&node.requests_to("/get_blocks.bin")[2]), 100);
        assert!(node.requests_to("/get_transaction_pool_hashes").is_empty());
        // It keeps happening: the plain list, from then on.
        for _ in 1..POOL_CHANGES_GIVE_UP {
            client.get_mempool_txids().await.unwrap_err();
        }
        assert_eq!(
            client.get_mempool_txids().await.unwrap(),
            vec![D.to_owned()]
        );
        assert_eq!(
            client.get_mempool_txids().await.unwrap(),
            vec![D.to_owned()]
        );
        assert_eq!(
            node.requests_to("/get_blocks.bin").len(),
            3 + POOL_CHANGES_GIVE_UP as usize
        );
    }

    /// A node that answers neither way is an error, never an empty pool.
    #[tokio::test]
    async fn a_pool_that_cannot_be_read_is_an_error() {
        let (client, _node) = scripted().await;
        client.get_mempool_txids().await.unwrap_err();
    }

    #[test]
    fn pool_answers_parse_in_every_shape_a_node_sends() {
        let (a, blob, _) = pruned_fixture();
        let full = parse_pool_changes(&pool_answer(POOL_INFO_FULL, 9, &[(&a, &blob)], &[B], &[C]))
            .unwrap()
            .unwrap();
        assert_eq!(
            full,
            PoolChanges {
                full: true,
                daemon_time: 9,
                added: vec![(a, blob)],
                added_ids: vec![B.to_owned()],
                removed: vec![C.to_owned()],
                ..PoolChanges::default()
            }
        );
        // Nothing changed: no lists at all.
        let quiet = parse_pool_changes(&pool_answer(POOL_INFO_INCREMENTAL, 10, &[], &[], &[]))
            .unwrap()
            .unwrap();
        assert_eq!(
            quiet,
            PoolChanges {
                daemon_time: 10,
                ..Default::default()
            }
        );
        // No extent, extent 0, no time, a failed status: not a description
        // of the pool.
        for fields in [
            vec![("status", V::Str(b"OK".to_vec()))],
            vec![
                ("status", V::Str(b"OK".to_vec())),
                ("pool_info_extent", V::U8(0)),
                ("daemon_time", V::U64(1)),
            ],
            vec![
                ("status", V::Str(b"OK".to_vec())),
                ("pool_info_extent", V::U8(1)),
            ],
            vec![
                ("status", V::Str(b"BUSY".to_vec())),
                ("pool_info_extent", V::U8(1)),
                ("daemon_time", V::U64(1)),
            ],
        ] {
            assert_eq!(parse_pool_changes(&epee(&fields)).unwrap(), None);
        }
        parse_pool_changes(b"not epee").unwrap_err();
    }

    /// The view of the pool: changes applied in order, a whole pool
    /// replacing it, the plain list correcting it, and bodies kept only for
    /// transactions still there.
    #[test]
    fn the_pool_view_applies_changes_and_corrections() {
        let (a, blob) = whole_fixture();
        let mut view = PoolView::default();
        view.apply(PoolChanges {
            full: true,
            daemon_time: 5,
            added: vec![(a.clone(), blob.clone())],
            added_ids: vec![B.to_owned()],
            removed: vec![],
            ..PoolChanges::default()
        });
        assert_eq!(view.since, 5);
        assert_eq!(sorted(view.list()), sorted(vec![a.clone(), B.to_owned()]));
        assert!(view.bodies.contains_key(&a));
        // A body under another id, or a pruned one (nothing to check it
        // against), is a known id without a body.
        let (_, pruned, _) = pruned_fixture();
        view.apply(PoolChanges {
            full: false,
            daemon_time: 5,
            added: vec![(D.to_owned(), blob), (C.to_owned(), pruned)],
            added_ids: vec![],
            removed: vec![],
            ..PoolChanges::default()
        });
        assert!(!view.bodies.contains_key(D));
        assert!(!view.bodies.contains_key(C));
        assert!(view.txids.contains(D) && view.txids.contains(C));
        view.apply(PoolChanges {
            full: false,
            daemon_time: 5,
            added: vec![],
            added_ids: vec![],
            removed: vec![C.to_owned(), D.to_owned()],
            ..PoolChanges::default()
        });
        // An undecodable body is a known id without a body.
        view.apply(PoolChanges {
            full: false,
            daemon_time: 6,
            added: vec![(C.to_owned(), vec![1, 2, 3])],
            added_ids: vec![],
            removed: vec![B.to_owned()],
            ..PoolChanges::default()
        });
        assert_eq!(sorted(view.list()), sorted(vec![a, C.to_owned()]));
        assert!(!view.bodies.contains_key(C));
        // The plain list drops what it doesn't have, body and all.
        view.resync(vec![C.to_owned(), D.to_owned()]);
        assert_eq!(
            sorted(view.list()),
            sorted(vec![C.to_owned(), D.to_owned()])
        );
        assert!(view.bodies.is_empty());
        assert_eq!(view.since, 6, "the list doesn't move the clock");
        // A whole pool replaces everything.
        view.apply(PoolChanges {
            full: true,
            daemon_time: 7,
            added: vec![],
            added_ids: vec![B.to_owned()],
            removed: vec![],
            ..PoolChanges::default()
        });
        assert_eq!(view.list(), vec![B.to_owned()]);
    }

    /// A pruned transaction is taken under an id only if it hashes to it
    /// (with the hash of its pruned part); where nothing can be computed,
    /// the id it was sent under stands; with neither, it is refused.
    #[test]
    fn a_transaction_blob_is_checked_against_the_id_it_came_under() {
        let (txid, blob, prunable) = pruned_fixture();
        let whole = hex::decode(FIXTURE_TX_HEX.trim()).unwrap();
        // Whole: hashed, whatever was claimed.
        assert_eq!(decode_tx_blob(&whole, None, None).unwrap().txid, txid);
        assert_eq!(
            decode_tx_blob(&whole, None, Some(&txid)).unwrap().txid,
            txid
        );
        let error = decode_tx_blob(&whole, None, Some(B)).unwrap_err();
        assert!(error.contains("hashes to"), "{error}");
        // Pruned, with the hash of the rest: computed, and checked.
        let fetched = decode_tx_blob(&blob, Some(&prunable), None).unwrap();
        assert_eq!(fetched.txid, txid);
        assert!(shared::monero_tx::is_pruned(&fetched.tx));
        assert_eq!(
            decode_tx_blob(&blob, Some(&prunable), Some(&txid))
                .unwrap()
                .txid,
            txid
        );
        decode_tx_blob(&blob, Some(&prunable), Some(B)).unwrap_err();
        decode_tx_blob(&blob, Some(&[7; 32]), Some(&txid)).unwrap_err();
        // Pruned, no hash of the rest (or the all-zero "none"): as claimed.
        assert_eq!(decode_tx_blob(&blob, None, Some(&txid)).unwrap().txid, txid);
        assert_eq!(
            decode_tx_blob(&blob, Some(&[0; 32]), Some(B)).unwrap().txid,
            B
        );
        // ...and refused with no usable id at all.
        decode_tx_blob(&blob, None, None).unwrap_err();
        decode_tx_blob(&blob, None, Some("not an id")).unwrap_err();
        decode_tx_blob(&[1, 2, 3], None, Some(&txid)).unwrap_err();
    }

    fn tx_entry(txid: &str, in_pool: bool, block_height: Option<u64>) -> Value {
        let mut entry = json!({ "tx_hash": txid, "in_pool": in_pool, "as_hex": "" });
        if let Some(height) = block_height {
            entry["block_height"] = json!(height);
        }
        entry
    }

    /// Several transactions are asked about in one request. Only an
    /// affirmative answer is passed on: a miss the node names, or an entry
    /// that places the transaction. A non-answer is left out.
    #[tokio::test]
    async fn several_transactions_are_located_in_one_request() {
        let (client, node) = scripted().await;
        let (a, _, _) = pruned_fixture();
        let unasked = "e".repeat(64);
        node.answer(
            "/get_transactions",
            json!({
                "status": "OK",
                "txs": [
                    tx_entry(&a, false, Some(77)),
                    tx_entry(B, true, None),
                    // Confirmed, but not saying where: a non-answer.
                    tx_entry(C, false, None),
                    // Not asked about.
                    tx_entry(&unasked, false, Some(1)),
                ],
                "missed_tx": [D, unasked],
            })
            .to_string(),
        );
        let asked = [a.clone(), B.to_owned(), C.to_owned(), D.to_owned()];
        let located = client.locate_transactions(&asked).await.unwrap();
        assert_eq!(located.get(&a), Some(&TxLocation::InBlock(77)));
        assert_eq!(located.get(B), Some(&TxLocation::InPool));
        assert_eq!(located.get(D), Some(&TxLocation::NotFound));
        assert_eq!(located.len(), 3, "{located:?}");
        let requests = node.requests_to("/get_transactions");
        assert_eq!(requests.len(), 1);
        let request: Value = serde_json::from_slice(&requests[0]).unwrap();
        assert_eq!(request["prune"], json!(true));
        assert_eq!(request["txs_hashes"].as_array().unwrap().len(), 4);

        // A transaction the node names both as missed and as found has not
        // been answered about.
        let (client, node) = scripted().await;
        node.answer(
            "/get_transactions",
            json!({ "status": "OK", "txs": [tx_entry(B, true, None)], "missed_tx": [B] })
                .to_string(),
        );
        assert!(client
            .locate_transactions(&[B.to_owned()])
            .await
            .unwrap()
            .is_empty());
    }

    /// Transactions fetched by id come pruned under their ids: one the node
    /// doesn't have is left out, one that can't be read is left out, and
    /// one that wasn't asked for is an error.
    #[tokio::test]
    async fn transactions_are_fetched_pruned_under_their_ids() {
        let (client, node) = scripted().await;
        let (a, blob, prunable) = pruned_fixture();
        let pruned_entry = |txid: &str, blob: &[u8]| {
            json!({
                "tx_hash": txid, "in_pool": true, "as_hex": "",
                "pruned_as_hex": hex::encode(blob), "prunable_hash": hex::encode(prunable),
            })
        };
        node.answer(
            "/get_transactions",
            json!({
                "status": "OK",
                "txs": [pruned_entry(&a, &blob), pruned_entry(C, &[1, 2, 3])],
                "missed_tx": [B],
            })
            .to_string(),
        );
        let asked = [a.clone(), B.to_owned(), C.to_owned()];
        let fetched = client.get_transactions_with_ids(&asked).await.unwrap();
        assert_eq!(fetched.len(), 1);
        assert_eq!(fetched[0].txid, a);
        assert!(shared::monero_tx::is_pruned(&fetched[0].tx));

        // The same transaction, found with where it is.
        let (client, node) = scripted().await;
        node.answer(
            "/get_transactions",
            json!({ "status": "OK", "txs": [pruned_entry(&a, &blob)] }).to_string(),
        );
        let (found, location) = client.find_transaction(&a).await.unwrap().unwrap();
        assert_eq!(
            (found.txid.as_str(), location),
            (a.as_str(), TxLocation::InPool)
        );
        assert_eq!(node.requests_to("/get_transactions").len(), 1);
        // Asked for one transaction and sent another: refused.
        client.find_transaction(B).await.unwrap_err();
        client
            .get_transactions_with_ids(&[B.to_owned()])
            .await
            .unwrap_err();
        // A named miss is "not found", in one request.
        let (client, node) = scripted().await;
        node.answer(
            "/get_transactions",
            json!({ "status": "OK", "missed_tx": [B] }).to_string(),
        );
        assert!(client.find_transaction(B).await.unwrap().is_none());
    }

    /// A small answer measures the link's round trip
    /// (`docs/engine_scaling.md` section 1): the link is measured from then on.
    #[tokio::test]
    async fn a_small_answer_measures_the_round_trip() {
        let (client, node) = scripted().await;
        let before = client.link().unwrap();
        assert!(!before.measured);
        assert_eq!(before.rtt_ms, 1000, "the starting guess");
        node.answer("/get_height", height_answer(101, B));
        client.get_tip().await.unwrap();
        let after = client.link().unwrap();
        assert!(after.measured);
        assert!(after.rtt_ms < 1000, "a local answer: {after:?}");
        assert!(after.last_measured_unix.is_some());
    }

    /// The pool outlook comes from `get_info` and the pool's stats; a node
    /// that refuses the stats still gives the count, and a median under
    /// the full-reward zone counts as the zone.
    #[tokio::test]
    async fn the_pool_outlook_reads_the_node_s_info_and_pool_stats() {
        async fn node(stats: bool) -> RpcDaemonClient {
            let app = axum::Router::new()
                .route(
                    "/json_rpc",
                    axum::routing::post(async || {
                        axum::Json(json!({ "jsonrpc": "2.0", "id": "0", "result": {
                            "status": "OK", "tx_pool_size": 7, "block_weight_median": 120_000
                        }}))
                    }),
                )
                .route(
                    "/get_transaction_pool_stats",
                    axum::routing::post(async move || {
                        if stats {
                            axum::Json(json!({ "status": "OK", "pool_stats": {
                                "bytes_total": 21_000, "txs_total": 9, "histo": []
                            }}))
                        } else {
                            axum::Json(json!({ "status": "Restricted" }))
                        }
                    }),
                );
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            RpcDaemonClient::new("127.0.0.1", port, false, false).unwrap()
        }
        assert_eq!(
            node(true).await.get_pool_outlook().await.unwrap(),
            Some(PoolOutlook {
                txs: 9,
                bytes: Some(21_000),
                penalty_free: PoolOutlook::FULL_REWARD_ZONE,
            })
        );
        let refused = node(false).await;
        assert_eq!(
            refused.get_pool_outlook().await.unwrap(),
            Some(PoolOutlook {
                txs: 7,
                bytes: None,
                penalty_free: PoolOutlook::FULL_REWARD_ZONE,
            })
        );
        // The page's requests are no sample, and a refusal no failure, for
        // the link the scan sizes its requests from.
        let link = refused.link().unwrap();
        assert_eq!(link.last_measured_unix, None, "{link:?}");
        assert_eq!(link.timeouts_last_hour, 0);
    }

    /// A request that runs out of time is a timeout, not any failure: the
    /// scan asks for less next time, and the link's rate estimate halves.
    #[tokio::test]
    async fn a_request_that_runs_out_of_time_says_so_and_slows_the_estimate() {
        let app = axum::Router::new().fallback(async || {
            tokio::time::sleep(Duration::from_secs(2)).await;
            "too late"
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RpcDaemonClient::new("127.0.0.1", port, false, false).unwrap();

        let err = client
            .post_timed(
                "/get_blocks.bin",
                "/get_blocks.bin",
                Vec::new(),
                false,
                Duration::from_millis(200),
                MAX_RESPONSE_BYTES,
            )
            .await
            .err()
            .unwrap();
        assert!(matches!(err, DaemonError::TimedOut(_)), "{err:?}");
        assert!(err.asks_for_less());
        let link = client.link().unwrap();
        assert_eq!(link.timeouts_last_hour, 1);
        assert_eq!(
            link.rate_bytes_per_sec,
            (crate::link::COLD_RATE_BYTES_PER_SEC / 2.0) as u64
        );
    }

    /// A block answer may be four times what its blocks are expected to
    /// weigh, and never less than the general cap: a node can't send an
    /// unbounded answer to a small request, and a big request isn't refused
    /// for being as big as asked.
    #[test]
    fn a_block_answer_is_capped_by_what_was_asked_for() {
        let client = RpcDaemonClient::new("127.0.0.1", 1, false, false).unwrap();
        // 50 kB a block until measured.
        assert_eq!(client.blocks_cap(1), MAX_RESPONSE_BYTES);
        assert_eq!(client.blocks_cap(1_000), 200_000_000);
    }

    /// An answer over the cap asks for less too.
    #[tokio::test]
    async fn an_answer_over_the_cap_asks_for_less() {
        let (client, node) = scripted().await;
        let client = client.with_max_response_bytes(10);
        node.answer("/get_height", height_answer(101, B));
        let err = client.get_height().await.unwrap_err();
        assert!(matches!(err, DaemonError::TooLarge(_)), "{err:?}");
        assert!(err.asks_for_less());
    }

    /// The tip's id comes with its height, unless it isn't an id.
    #[tokio::test]
    async fn the_tip_comes_with_its_id_when_the_node_gives_one() {
        for (hash, expected) in [
            (json!(B.to_uppercase()), Some(B.to_owned())),
            (json!("nonsense"), None),
            (Value::Null, None),
        ] {
            let (client, node) = scripted().await;
            let mut answer = json!({ "status": "OK", "height": 101 });
            if !hash.is_null() {
                answer["hash"] = hash;
            }
            node.answer("/get_height", answer.to_string());
            assert_eq!(
                client.get_tip().await.unwrap(),
                ChainTip {
                    height: 100,
                    hash: expected
                }
            );
        }
    }

    /// A round's two questions in one request. The first time the client
    /// knows no tip: the pool, then the tip. After that the poll for the
    /// pool's changes names the tip it last saw, and while the node answers
    /// with no blocks the tip stands and `/get_height` isn't asked. A block
    /// in the answer means the tip moved: then it is asked for, once.
    #[tokio::test]
    async fn the_tip_and_the_pool_are_one_request_while_the_chain_has_not_moved() {
        let (client, node) = scripted().await;
        node.answer("/get_height", height_answer(101, C));
        node.answer("/get_height", height_answer(102, D));
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_FULL, 100, &[], &[B], &[]),
        );
        node.answer("/get_blocks.bin", pool_and_chain_answer(105, 101, 0));
        node.answer("/get_blocks.bin", pool_and_chain_answer(110, 102, 1));
        node.answer("/get_blocks.bin", pool_and_chain_answer(115, 102, 0));
        let ask = async || {
            let (tip, pool) = client.get_tip_and_mempool().await;
            (tip.unwrap(), pool.unwrap())
        };
        let asked = |path: &str| node.requests_to(path).len();

        // Nothing known: the pool, then the tip.
        assert_eq!(ask().await, (tip(100, C), vec![B.to_owned()]));
        assert_eq!((asked("/get_blocks.bin"), asked("/get_height")), (1, 1));
        // The tip hasn't moved: one request, naming it.
        assert_eq!(ask().await, (tip(100, C), vec![B.to_owned()]));
        assert_eq!((asked("/get_blocks.bin"), asked("/get_height")), (2, 1));
        // It has: the node sends a block, and is asked for its tip.
        assert_eq!(ask().await, (tip(101, D), vec![B.to_owned()]));
        assert_eq!((asked("/get_blocks.bin"), asked("/get_height")), (3, 2));
        // The new tip stands in turn.
        assert_eq!(ask().await, (tip(101, D), vec![B.to_owned()]));
        assert_eq!((asked("/get_blocks.bin"), asked("/get_height")), (4, 2));

        let polls = node.requests_to("/get_blocks.bin");
        assert_eq!(
            polls.iter().map(|r| tip_named(r)).collect::<Vec<_>>(),
            vec![
                None,
                Some(C.to_owned()),
                Some(C.to_owned()),
                Some(D.to_owned())
            ]
        );
        assert_eq!(
            polls.iter().map(|r| since_of(r)).collect::<Vec<_>>(),
            vec![0, 100, 105, 110],
            "the pool is followed by its changes all the while"
        );
        assert_eq!(asked("/get_transaction_pool_hashes"), 0);
        let requests = |endpoint: &str| {
            client
                .stats()
                .iter()
                .find(|s| s.endpoint == endpoint)
                .map_or(0, |s| s.requests)
        };
        assert_eq!(requests(POOL_CHANGES_ENDPOINT), 1);
        assert_eq!(requests(POOL_CHANGES_AND_TIP_ENDPOINT), 3);
        // A plain poll of the pool never names a tip.
        client.get_mempool_txids().await.unwrap();
        assert_eq!(tip_named(&node.requests_to("/get_blocks.bin")[4]), None);
    }

    /// A node that doesn't answer the two questions together as monerod
    /// does (no description of the pool, a run of blocks where one was
    /// asked for, a length that doesn't match the tip named) is asked them
    /// apart from then on. Its pool is still followed by its changes.
    #[tokio::test]
    async fn a_node_that_cannot_answer_for_tip_and_pool_together_is_asked_apart() {
        for odd_answer in [
            epee(&[("status", V::Str(b"Failed".to_vec()))]),
            b"<html>502 Bad Gateway</html>".to_vec(),
            pool_and_chain_answer(105, 101, TIP_MOVED_BLOCKS_MAX + 1),
            pool_and_chain_answer(105, 0, 0),
            pool_and_chain_answer(105, 250, 0),
        ] {
            let (client, node) = scripted().await;
            node.answer("/get_height", height_answer(101, C));
            node.answer(
                "/get_blocks.bin",
                pool_answer(POOL_INFO_FULL, 100, &[], &[B], &[]),
            );
            node.answer("/get_blocks.bin", odd_answer);
            node.answer(
                "/get_blocks.bin",
                pool_answer(POOL_INFO_INCREMENTAL, 110, &[], &[D], &[]),
            );
            let ask = async || {
                let (tip, pool) = client.get_tip_and_mempool().await;
                (tip.unwrap(), pool.unwrap())
            };
            ask().await;
            // The odd answer: the tip is asked for, and the pool is still
            // read (from that answer if it described the pool, else from a
            // second, plain request for its changes).
            let (tip_now, _) = ask().await;
            assert_eq!(tip_now, tip(100, C));
            assert_eq!(node.requests_to("/get_height").len(), 2);
            // From then on: the pool's changes and the tip, apart.
            let (_, pool) = ask().await;
            assert!(pool.contains(&D.to_owned()), "{pool:?}");
            assert_eq!(node.requests_to("/get_height").len(), 3);
            let polls = node.requests_to("/get_blocks.bin");
            assert_eq!(tip_named(&polls[1]), Some(C.to_owned()));
            assert!(
                polls[2..].iter().all(|r| tip_named(r).is_none()),
                "asked together again"
            );
            assert!(node.requests_to("/get_transaction_pool_hashes").is_empty());
        }
    }

    /// The tip and the pool are two answers: a node whose pool can't be
    /// read still gives its tip, and one with no tip still gives its pool.
    #[tokio::test]
    async fn a_tip_and_a_pool_asked_for_together_fail_apart() {
        let (client, node) = scripted().await;
        node.answer("/get_height", height_answer(101, C));
        let (tip_now, pool) = client.get_tip_and_mempool().await;
        assert_eq!(tip_now.unwrap(), tip(100, C));
        pool.unwrap_err();

        let (client, node) = scripted().await;
        node.answer(
            "/get_blocks.bin",
            pool_answer(POOL_INFO_FULL, 100, &[], &[B], &[]),
        );
        let (tip_now, pool) = client.get_tip_and_mempool().await;
        tip_now.unwrap_err();
        assert_eq!(pool.unwrap(), vec![B.to_owned()]);
    }

    fn header(height: u64, hash: &str, prev_hash: &str) -> Value {
        json!({ "height": height, "hash": hash, "prev_hash": prev_hash, "timestamp": 1_000 + height,
            "block_weight": 1_000 * height, "num_txes": height })
    }

    fn rpc_result(result: &Value) -> String {
        json!({ "id": "0", "jsonrpc": "2.0", "result": result }).to_string()
    }

    /// A large block's outline comes from `get_block`: its id computed from
    /// the blob, its parent and time, and its transactions' ids in order,
    /// with no transaction fetched. A blob from another height is refused
    /// (`docs/engine_scaling.md` section 4).
    #[tokio::test]
    async fn a_large_blocks_outline_is_read_from_its_own_blob() {
        let (client, node) = scripted().await;
        let mut block: monero::Block =
            deserialize(&hex::decode(tests::COINBASE_ONLY_BLOCK_HEX).unwrap()).unwrap();
        block.tx_hashes = vec![monero::Hash([1; 32]), monero::Hash([2; 32])];
        let height = match &block.miner_tx.prefix.inputs[0] {
            monero::blockdata::transaction::TxIn::Gen { height } => height.0,
            monero::blockdata::transaction::TxIn::ToKey {
                amount: _,
                key_offsets: _,
                k_image: _,
            } => unreachable!("a coinbase input"),
        };
        let blob = hex::encode(serialize(&block));
        node.answer(
            "/json_rpc",
            rpc_result(&json!({ "status": "OK", "blob": blob })),
        );
        let outline = client.get_block_outline(height, Some(2)).await.unwrap();
        assert_eq!(outline.height, height);
        assert_eq!(outline.hash, hex::encode(block.id().0));
        assert_eq!(outline.prev_hash, hex::encode(block.header.prev_id.0));
        assert_eq!(outline.timestamp, block.header.timestamp.0);
        assert_eq!(
            outline.txids,
            vec![hex::encode([1; 32]), hex::encode([2; 32])]
        );
        let request: Value = serde_json::from_slice(&node.requests_to("/json_rpc")[0]).unwrap();
        assert_eq!(request["method"], json!("get_block"));
        assert_eq!(request["params"], json!({ "height": height }));
        assert!(node.requests_to("/get_transactions").is_empty());
        // The node answered with another height's block.
        let error = client
            .get_block_outline(height + 1, Some(2))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("sent another"), "{error}");
    }

    /// What proof-of-work checking reads (`docs/proof_of_work.md)`: a block's
    /// blob as `get_block` sends it, and headers with their difficulties,
    /// whose two 64-bit halves are joined; a header from another height is
    /// refused.
    #[tokio::test]
    async fn blobs_and_difficulty_headers_are_read_as_monerod_sends_them() {
        let (client, node) = scripted().await;
        let blob = tests::COINBASE_ONLY_BLOCK_HEX;
        node.answer(
            "/json_rpc",
            rpc_result(&json!({ "status": "OK", "blob": blob, "json": "{}" })),
        );
        assert_eq!(
            client.get_block_blob(7).await.unwrap(),
            hex::decode(blob).unwrap()
        );
        let request: Value = serde_json::from_slice(&node.requests_to("/json_rpc")[0]).unwrap();
        assert_eq!(request["method"], json!("get_block"));
        assert_eq!(request["params"], json!({ "height": 7 }));

        let (client, node) = scripted().await;
        let mut wide = header(6, C, B);
        wide["difficulty"] = json!(5u64);
        wide["difficulty_top64"] = json!(1u64);
        wide["cumulative_difficulty"] = json!(9u64);
        wide["cumulative_difficulty_top64"] = json!(2u64);
        let mut narrow = header(5, B, D);
        narrow["difficulty"] = json!(700u64);
        narrow["cumulative_difficulty"] = json!(u64::MAX);
        node.answer(
            "/json_rpc",
            rpc_result(&json!({ "status": "OK", "headers": [narrow, wide] })),
        );
        let headers = client.get_difficulty_headers(5, 2).await.unwrap();
        assert_eq!(
            headers,
            vec![
                DifficultyHeader {
                    height: 5,
                    hash: B.to_owned(),
                    prev_hash: D.to_owned(),
                    timestamp: 1_005,
                    difficulty: 700,
                    cumulative_difficulty: u128::from(u64::MAX),
                },
                DifficultyHeader {
                    height: 6,
                    hash: C.to_owned(),
                    prev_hash: B.to_owned(),
                    timestamp: 1_006,
                    difficulty: (1u128 << 64) + 5,
                    cumulative_difficulty: (2u128 << 64) + 9,
                },
            ]
        );
        let request: Value = serde_json::from_slice(&node.requests_to("/json_rpc")[0]).unwrap();
        assert_eq!(request["method"], json!("get_block_headers_range"));
        assert_eq!(
            request["params"],
            json!({ "start_height": 5, "end_height": 6 })
        );

        let (client, node) = scripted().await;
        node.answer(
            "/json_rpc",
            rpc_result(&json!({ "status": "OK", "headers": [header(9, B, D)] })),
        );
        let error = client.get_difficulty_headers(5, 1).await.unwrap_err();
        assert!(error.to_string().contains("sent another"), "{error}");
    }

    /// Headers come a range at a time; a header for another height than the
    /// one asked for is refused; and a lone hash is asked for as just that.
    #[tokio::test]
    async fn headers_and_hashes_are_asked_for_as_just_that() {
        let (client, node) = scripted().await;
        node.answer(
            "/json_rpc",
            rpc_result(&json!({ "status": "OK", "headers": [header(5, B, D), header(6, C, B)] })),
        );
        let headers = client.get_chain_headers(5, 2).await.unwrap();
        assert_eq!(
            headers,
            vec![
                ChainHeader {
                    height: 5,
                    hash: B.to_owned(),
                    prev_hash: D.to_owned(),
                    timestamp: 1_005,
                    weight: Some(5_000),
                    tx_count: Some(5),
                },
                ChainHeader {
                    height: 6,
                    hash: C.to_owned(),
                    prev_hash: B.to_owned(),
                    timestamp: 1_006,
                    weight: Some(6_000),
                    tx_count: Some(6),
                },
            ]
        );
        let request: Value = serde_json::from_slice(&node.requests_to("/json_rpc")[0]).unwrap();
        assert_eq!(request["method"], json!("get_block_headers_range"));
        assert_eq!(
            request["params"],
            json!({ "start_height": 5, "end_height": 6 })
        );
        // The node answered from another height.
        client.get_chain_headers(9, 2).await.unwrap_err();
        assert!(client.get_chain_headers(5, 0).await.unwrap().is_empty());

        let (client, node) = scripted().await;
        node.answer("/json_rpc", rpc_result(&json!(B)));
        assert_eq!(client.get_block_hash(7).await.unwrap(), B);
        let request: Value = serde_json::from_slice(&node.requests_to("/json_rpc")[0]).unwrap();
        assert_eq!(request["method"], json!("on_get_block_hash"));
        assert_eq!(request["params"], json!([7]));
        let (client, node) = scripted().await;
        node.answer("/json_rpc", rpc_result(&json!("not a hash")));
        client.get_block_hash(7).await.unwrap_err();
    }

    /// A whole `get_blocks.bin` entry with pruned transactions: each
    /// transaction's id is the one the block lists for it, checked against
    /// the pruned body and the hash of the rest.
    #[test]
    fn a_blocks_pruned_transactions_are_checked_against_its_own_list() {
        let (txid, blob, prunable) = pruned_fixture();
        let template: monero::Block =
            deserialize(&hex::decode(tests::COINBASE_ONLY_BLOCK_HEX).unwrap()).unwrap();
        let height = match &template.miner_tx.prefix.inputs[0] {
            monero::blockdata::transaction::TxIn::Gen { height } => height.0,
            monero::blockdata::transaction::TxIn::ToKey {
                amount: _,
                key_offsets: _,
                k_image: _,
            } => unreachable!("a coinbase input"),
        };
        let block_listing = |listed: &str| {
            let mut block = template.clone();
            block.tx_hashes = vec![monero::Hash::from_slice(&hex::decode(listed).unwrap())];
            serialize(&block)
        };
        let entry = |listed: &str, prunable_hash| BinBlock {
            block: Some(block_listing(listed)),
            txs: vec![BinTx {
                blob: blob.clone(),
                prunable_hash,
            }],
        };
        let block = entry(&txid, Some(prunable))
            .into_chain_block(height)
            .unwrap();
        // Kept as the scan reads it: no ring members, but the key images.
        assert!(block.txs[0].input.prefix().inputs.is_empty());
        assert!(!block.txs[0].key_images.is_empty());
        assert!(block.wire_bytes > blob.len() as u64);
        assert_eq!(block.txids, vec![txid.clone()]);
        // The block lists another transaction than the one that came.
        let error = entry(B, Some(prunable))
            .into_chain_block(height)
            .unwrap_err();
        assert!(error.to_string().contains("hashes to"), "{error}");
        // No hash of the pruned part: the block's list is what names it.
        assert_eq!(
            entry(&txid, None).into_chain_block(height).unwrap().txids,
            vec![txid]
        );
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

    /// The test node, or the mainnet node named by `ENGINE_LIVE_TEST_NODE`
    /// (`host:port`, TLS) when the usual one is down or behind.
    fn client() -> RpcDaemonClient {
        let named = std::env::var("ENGINE_LIVE_TEST_NODE").ok();
        let (host, port) = match named.as_deref().and_then(|node| node.rsplit_once(':')) {
            Some((host, port)) => (host, port.parse().expect("a port number")),
            None => (TEST_NODE_HOST, TEST_NODE_PORT),
        };
        RpcDaemonClient::new(host, port, true, true).unwrap()
    }

    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
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
    #[ignore = "needs a live mainnet node"]
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
    #[ignore = "needs a live mainnet node"]
    async fn real_node_header_matches_a_known_immutable_block() {
        // Same block as `real_node_get_block_hash_matches_a_known_immutable_block`
        // above - captured live against this exact node while building this
        // client.
        let headers = client().get_chain_headers(3_755_690, 1).await.unwrap();
        assert_eq!(headers.len(), 1);
        assert_eq!(headers[0].timestamp, 1_788_593_344);
        assert_eq!(
            headers[0].hash,
            "61dcf348728fd124895e5e9e5188cc34a13c483f84ddfb5d3998f38d0ae55aa4"
        );
    }

    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_block_transactions_all_parse_as_valid_monero_transactions() {
        // Proves the real deserialization path handles *current* mainnet
        // transaction formats (view tags, CLSAG, bulletproofs+) - the crate's own
        // fixture used elsewhere in this codebase is a single, older-format
        // transaction and wouldn't catch a version-compatibility regression here.
        // 87 non-coinbase transactions, captured live against this exact block.
        let blocks = client().get_chain_blocks(3_755_690, 1).await.unwrap();
        let block = &blocks[0];
        assert_eq!(block.txs.len(), 87);
        assert_eq!(block.txids.len(), 87);
        for tx in &block.txs {
            assert!(!tx.key_images.is_empty());
            assert!(!tx.input.prefix().outputs.is_empty());
        }
        assert!(block.txids.iter().any(|txid| txid == KNOWN_TX));
    }

    /// A mainnet transaction in block 3,755,690.
    const KNOWN_TX: &str = "24f70768d285ca14dd8080a9cddf1ecdebce7553933c9a638090b5fda2101fa8";

    /// The blocks of a range with their transactions whole, asked for with
    /// a request of the test's own: what the pruned ones are held against.
    async fn whole_blocks(c: &RpcDaemonClient, start: u64, count: u64) -> Vec<Vec<Transaction>> {
        let request = epee_request(&[
            ("prune", EpeeField::Bool(false)),
            ("start_height", EpeeField::U64(start)),
            ("max_block_count", EpeeField::U64(count)),
        ]);
        let response = c
            .post("/get_blocks.bin", "/get_blocks.bin", request, false)
            .await
            .unwrap();
        parse_get_blocks_bin_response(&response)
            .unwrap()
            .iter()
            .map(|block| {
                block
                    .txs
                    .iter()
                    .map(|tx| deserialize(&tx.blob).unwrap())
                    .collect()
            })
            .collect()
    }

    /// What the scanner reads blocks with, against mainnet blocks full of
    /// every current transaction shape: the pruned transactions carry the
    /// ids their whole forms hash to, each block names itself as
    /// `on_get_block_hash` names it, and the headers say the same.
    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_pruned_chain_blocks_match_the_whole_ones() {
        let c = client();
        let (start, count) = (3_755_688, 4);
        let chain = c.get_chain_blocks(start, count).await.unwrap();
        let whole = whole_blocks(&c, start, count).await;
        assert_eq!(chain.len() as u64, count);
        let mut transactions = 0;
        for (offset, block) in chain.iter().enumerate() {
            let height = start + offset as u64;
            assert_eq!(block.height, height);
            assert_eq!(block.hash, c.get_block_hash(height).await.unwrap());
            let whole = &whole[offset];
            assert_eq!(block.txs.len(), whole.len());
            for (index, (pruned, whole)) in block.txs.iter().zip(whole).enumerate() {
                assert_eq!(
                    block.txids[index],
                    hex::encode(whole.hash().to_bytes()),
                    "block {height}, transaction {index}"
                );
                // What the scan keeps is what the whole transaction says.
                assert_eq!(*pruned, crate::daemon::ScanTx::of(whole));
                transactions += 1;
            }
        }
        assert!(transactions > 0, "blocks with transactions in them");
        assert_eq!(
            c.get_chain_headers(start, count).await.unwrap(),
            chain.iter().map(ChainBlock::header).collect::<Vec<_>>()
        );
        // The genesis block, the ordinary way, then block 1 from the batch.
        let first = c.get_chain_blocks(0, 2).await.unwrap();
        assert_eq!((first[0].height, first[1].height), (0, 1));
        assert_eq!(first[1].prev_hash, first[0].hash);
        assert_eq!(first[0].hash, c.get_block_hash(0).await.unwrap());
    }

    /// The pool as a real node describes it: followed by its changes, the
    /// bodies that came with them handed over under their ids.
    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_pool_is_followed_by_its_changes() {
        let c = client();
        let pool = c.get_mempool_txids().await.unwrap();
        let stats = c.stats();
        assert!(
            stats
                .iter()
                .any(|s| s.endpoint == POOL_CHANGES_ENDPOINT && s.requests == 1),
            "{stats:?}"
        );
        assert!(
            !stats
                .iter()
                .any(|s| s.endpoint == "/get_transaction_pool_hashes"),
            "the node described its pool: the plain list wasn't needed"
        );
        let asked: Vec<String> = pool.iter().take(150).cloned().collect();
        let bodies = c.get_transactions_with_ids(&asked).await.unwrap();
        assert!(bodies.iter().all(|body| asked.contains(&body.txid)));
        let tip = c.get_tip().await.unwrap();
        assert_eq!(tip.hash.map(|hash| hash.len()), Some(64));
    }

    /// The tip and the pool's changes in one request, as a real node
    /// answers it: while the chain ends where it did, the node says so with
    /// the changes and isn't asked for the tip.
    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_says_the_tip_has_not_moved_with_the_pools_changes() {
        let c = client().with_pool_timing(Duration::ZERO, Duration::from_secs(3600));
        let requests = |endpoint: &str| {
            c.stats()
                .iter()
                .find(|s| s.endpoint == endpoint)
                .map_or(0, |s| s.requests)
        };
        // Nothing known yet: the pool, then the tip.
        let (first, pool) = c.get_tip_and_mempool().await;
        let first = first.unwrap();
        pool.unwrap();
        assert_eq!(requests("/get_height"), 1);
        assert_eq!(requests(POOL_CHANGES_ENDPOINT), 1);
        // Blocks are minutes apart: of a few polls in a row, at least one
        // finds the tip where it was, and that one costs one request.
        let mut unmoved = 0;
        for _ in 0..3 {
            let (height_asks, both_asks) = (
                requests("/get_height"),
                requests(POOL_CHANGES_AND_TIP_ENDPOINT),
            );
            let (tip, pool) = c.get_tip_and_mempool().await;
            let tip = tip.unwrap();
            pool.unwrap();
            assert_eq!(requests(POOL_CHANGES_AND_TIP_ENDPOINT), both_asks + 1);
            assert!(tip.height >= first.height);
            assert_eq!(tip.hash.as_ref().map(String::len), Some(64));
            if requests("/get_height") == height_asks {
                unmoved += 1;
                // And it is the tip: the node names the same block.
                assert_eq!(c.get_block_hash(tip.height).await.ok(), tip.hash);
            }
        }
        assert!(unmoved > 0, "the tip moved on every poll");
        assert_eq!(requests(POOL_CHANGES_ENDPOINT), 1);
        assert_eq!(requests("/get_transaction_pool_hashes"), 0);
    }

    /// Pool transactions fetched by id (as for a node that can't say pool
    /// changes, or past the bodies it sends with them) come pruned, with the
    /// hash of the rest, and hash to the ids they were asked for by.
    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_pool_transactions_fetched_by_id_hash_to_their_ids() {
        let c = client();
        let pool: Vec<String> = c
            .pool_hashes()
            .await
            .unwrap()
            .into_iter()
            .take(20)
            .collect();
        assert!(!pool.is_empty(), "needs a non-empty live mempool");
        let resp = c.request_transactions(&pool).await.unwrap();
        let entries = resp.txs.unwrap_or_default();
        assert!(!entries.is_empty());
        for entry in &entries {
            assert_ne!(entry.prunable_hash, "0".repeat(64), "a real hash");
            assert_eq!(entry.prunable_hash.len(), 64);
            let fetched = entry.fetched().unwrap();
            assert!(pool.contains(&fetched.txid));
            assert!(shared::monero_tx::is_pruned(&fetched.tx));
        }
    }

    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_locate_transaction_finds_a_known_confirmed_tx() {
        let location = client().locate_transaction(KNOWN_TX).await.unwrap();
        assert_eq!(location, TxLocation::InBlock(3_755_690));
    }

    /// A transaction looked up by its id (the admin payment lookup) is the
    /// one its block holds, placed at that block.
    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_find_transaction_matches_the_same_tx_in_its_block() {
        let c = client();
        let (fetched, location) = c.find_transaction(KNOWN_TX).await.unwrap().unwrap();
        assert_eq!(location, TxLocation::InBlock(3_755_690));
        assert_eq!(fetched.txid, KNOWN_TX);
        let block = c.get_chain_blocks(3_755_690, 1).await.unwrap().remove(0);
        let index = block
            .txids
            .iter()
            .position(|txid| txid == KNOWN_TX)
            .expect("the known txid must be one of this block's own transactions");
        assert_eq!(crate::daemon::ScanTx::of(&fetched.tx), block.txs[index]);
    }

    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_find_transaction_finds_nothing_for_a_bogus_hash() {
        let found = client()
            .find_transaction("0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .unwrap();
        assert!(found.is_none());
    }

    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_locate_transaction_reports_not_found_for_a_bogus_hash() {
        let location = client()
            .locate_transaction("0000000000000000000000000000000000000000000000000000000000000000")
            .await
            .unwrap();
        assert_eq!(location, TxLocation::NotFound);
    }

    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_is_key_image_spent_reports_unspent_for_a_null_image() {
        let statuses = client()
            .is_key_image_spent(&[
                "0000000000000000000000000000000000000000000000000000000000000000".to_owned(),
            ])
            .await
            .unwrap();
        assert_eq!(statuses, vec![KeyImageStatus::Unspent]);
    }

    #[tokio::test]
    #[ignore = "needs a live mainnet node"]
    async fn real_node_end_to_end_scan_of_live_mempool_never_panics_and_finds_no_false_matches() {
        // The fullest available proof this pipeline works: real transactions,
        // fresh off a real node's real mempool, run through the actual
        // scanner's scan-and-record path (real KeyCustody scan +
        // real Store) against a wallet that has never received anything. Expect
        // zero matches (this key owns nothing) - the point is that scanning
        // diverse, unpredictable real-world transaction shapes never errors or
        // panics, which a single canned fixture can't prove.
        use crate::key_custody::{KeyCustody as _, PlainKeyCustody, WalletMaterial};
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
                &crate::store::NewTenant {
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

        // The pool as the scanner reads it: its ids, then the transactions
        // under those ids, pruned.
        let c = client();
        let pool = c.get_mempool_txids().await.unwrap();
        let txs = c.get_transactions_with_ids(&pool).await.unwrap();
        assert!(
            !txs.is_empty(),
            "test needs a non-empty live mempool to be meaningful"
        );

        for FetchedTx { txid, tx } in &txs {
            let scan = crate::scanner::scan_transaction_as(&key_custody, handle, txid, tx, 0..1)
                .await
                .unwrap();
            let touched =
                crate::scanner::record_scan_match(&store, &created.tenant.id, &scan, 0, None)
                    .unwrap();
            assert!(touched.is_empty());
        }
    }

    #[tokio::test]
    async fn a_response_larger_than_the_cap_is_refused_as_a_node_error() {
        use axum::routing::post;
        // Any endpoint: a valid-looking answer padded to 10kB.
        let app = axum::Router::new().fallback(post(async || {
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
