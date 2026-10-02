//! One point of `cargo xtask stress rounds` (docs/engine_stress.md): the
//! production scheduler catching up a backlog of blocks from a scripted node
//! whose link has a set round trip, time to first byte and rate, with
//! rounds of a set length run back to back as the engine's own loop runs
//! them. It measures what the round's length costs:
//!
//! - **throughput**: blocks scanned for each group a second;
//! - **refetches**: blocks the node sent against the blocks that were
//!   needed (a prefetched block dropped at the end of a round is sent again);
//! - **the wait between rounds**: settlement and the mempool get a turn once
//!   a round, so the longest round is the longest they wait;
//! - **a round's fixed cost**: how long a round with nothing to do takes;
//! - **discarded bytes**: blocks the scan's cache let go of unread.
//!
//! Progress is read over the binary's own SQLite connection, as in
//! `stress_fixture`.

mod stress_common;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::daemon::{
    ChainBlock, DaemonError, FetchedTx, KeyImageStatus, MoneroDaemonClient, ScanTx, TxLocation,
};
use engine::key_custody::{KeyCustody, PlainKeyCustody, SubaddressIndex, WalletHandle};
use engine::store::{Db, NewOrder, NewTenant, Store, TenantId};
use engine::work::{RoundInputs, ScanState, Tier};
use monero::{Network, Transaction};
use parking_lot::Mutex;
use serde_json::json;

/// The result format `xtask/src/stress.rs` reads.
const SCHEMA_VERSION: u64 = 1;
const NETWORK: &str = "mainnet";
/// Seconds between scripted blocks, as on mainnet.
const BLOCK_SECS: u64 = 120;

/// A node link: what each request costs in time.
#[derive(Clone, Copy)]
struct LinkModel {
    rtt: Duration,
    ttfb_per_block: Duration,
    /// Bytes a second; `None` sends at once and isn't reported, like a
    /// client that doesn't measure.
    rate: Option<f64>,
}

impl LinkModel {
    /// How long a request for `blocks` blocks of `bytes` bytes in all takes.
    fn blocks(&self, blocks: u64, bytes: u64) -> Duration {
        let transfer = self.rate.map_or(Duration::ZERO, |rate| {
            Duration::from_secs_f64(bytes as f64 / rate)
        });
        self.rtt + self.ttfb_per_block * u32::try_from(blocks).unwrap_or(u32::MAX) + transfer
    }
}

/// A scripted chain at a fixed height, served over a [`LinkModel`].
struct SweepDaemon {
    tip: u64,
    /// The tip's timestamp; block `h` is `(tip - h)` block times before it.
    tip_time: u64,
    block_bytes: u64,
    tx: ScanTx,
    txid: String,
    full_tx: Transaction,
    link: LinkModel,
    blocks_served: AtomicU64,
    bytes_served: AtomicU64,
    block_requests: AtomicU64,
    small_requests: AtomicU64,
    heights_served: Mutex<HashSet<u64>>,
}

impl SweepDaemon {
    fn hash(height: u64) -> String {
        format!("sweep-{height}")
    }

    async fn small(&self) {
        self.small_requests.fetch_add(1, Ordering::Relaxed);
        tokio::time::sleep(self.link.rtt).await;
    }
}

#[async_trait::async_trait]
impl MoneroDaemonClient for SweepDaemon {
    fn transfer_rate(&self) -> Option<f64> {
        self.link.rate
    }
    fn chain_blocks_timeout(&self, count: u64) -> Duration {
        engine::link::timeout_for(self.link.blocks(count, count * self.block_bytes))
    }
    fn transfer_timeout(&self, bytes: u64) -> Duration {
        engine::link::timeout_for(self.link.blocks(0, bytes))
    }
    async fn get_height(&self) -> Result<u64, DaemonError> {
        self.small().await;
        Ok(self.tip)
    }
    async fn get_block_hash(&self, height: u64) -> Result<String, DaemonError> {
        self.small().await;
        Ok(Self::hash(height))
    }
    async fn get_chain_blocks(
        &self,
        start_height: u64,
        count: u64,
    ) -> Result<Vec<ChainBlock>, DaemonError> {
        let heights: Vec<u64> = (start_height..start_height.saturating_add(count))
            .take_while(|height| *height <= self.tip)
            .collect();
        let served = heights.len() as u64;
        tokio::time::sleep(self.link.blocks(served, served * self.block_bytes)).await;
        self.block_requests.fetch_add(1, Ordering::Relaxed);
        self.blocks_served.fetch_add(served, Ordering::Relaxed);
        self.bytes_served
            .fetch_add(served * self.block_bytes, Ordering::Relaxed);
        self.heights_served.lock().extend(heights.iter().copied());
        Ok(heights
            .into_iter()
            .map(|height| ChainBlock {
                height,
                hash: Self::hash(height),
                prev_hash: height.checked_sub(1).map(Self::hash).unwrap_or_default(),
                timestamp: self.tip_time - (self.tip - height) * BLOCK_SECS,
                txs: vec![self.tx.clone()],
                txids: vec![self.txid.clone()],
                wire_bytes: self.block_bytes,
            })
            .collect())
    }
    async fn get_mempool_txids(&self) -> Result<Vec<String>, DaemonError> {
        self.small().await;
        Ok(vec![])
    }
    async fn get_transactions_with_ids(
        &self,
        txids: &[String],
    ) -> Result<Vec<FetchedTx>, DaemonError> {
        self.small().await;
        Ok(txids
            .iter()
            .filter(|txid| **txid == self.txid)
            .map(|txid| FetchedTx {
                txid: txid.clone(),
                tx: self.full_tx.clone(),
            })
            .collect())
    }
    async fn locate_transaction(&self, txid: &str) -> Result<TxLocation, DaemonError> {
        self.small().await;
        Ok(if txid == self.txid {
            TxLocation::InBlock(self.tip)
        } else {
            TxLocation::NotFound
        })
    }
    async fn is_key_image_spent(
        &self,
        images: &[String],
    ) -> Result<Vec<KeyImageStatus>, DaemonError> {
        self.small().await;
        Ok(vec![KeyImageStatus::Unspent; images.len()])
    }
}

struct Args(Vec<String>);

impl Args {
    fn value(&self, name: &str) -> Option<&str> {
        self.0
            .windows(2)
            .find(|pair| pair[0] == name)
            .map(|pair| pair[1].as_str())
    }
    fn get<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, Box<dyn Error>>
    where
        T::Err: Error + 'static,
    {
        Ok(self
            .value(name)
            .map(str::parse)
            .transpose()?
            .unwrap_or(default))
    }
}

/// Where each group's tenants start, and the tenants in it.
struct Group {
    start: u64,
    tenants: Vec<TenantId>,
}

/// The lowest cursor of each group's tenants.
fn group_cursors(conn: &rusqlite::Connection, groups: &[Group]) -> rusqlite::Result<Vec<u64>> {
    let mut cursors: HashMap<String, u64> = HashMap::new();
    let mut query = conn.prepare(
        "SELECT id, COALESCE(scanned_through_height, 0) FROM tenants WHERE network = ?1",
    )?;
    let rows = query.query_map([NETWORK], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (id, cursor) = row?;
        cursors.insert(id, cursor.max(0) as u64);
    }
    Ok(groups
        .iter()
        .map(|group| {
            group
                .tenants
                .iter()
                .filter_map(|id| cursors.get(id.as_str()).copied())
                .min()
                .unwrap_or(group.start)
        })
        .collect())
}

fn quantile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

async fn sweep() -> Result<(), Box<dyn Error>> {
    let args = Args(std::env::args().collect());
    let db_path: String = args.value("--db").ok_or("missing --db")?.to_owned();
    let tenant_count: usize = args.get("--tenants", 1)?;
    let group_count: usize = args.get("--groups", 1)?.max(1);
    let backlog: u64 = args.get("--backlog-blocks", 200)?;
    let round_budget = Duration::from_millis(args.get("--round-budget-ms", 10_000)?);
    let link_kbps: u64 = args.get("--link-kbps", 0)?;
    let link = LinkModel {
        rtt: Duration::from_millis(args.get("--rtt-ms", 50)?),
        ttfb_per_block: Duration::from_micros(args.get("--ttfb-us-per-block", 500)?),
        rate: (link_kbps > 0).then(|| link_kbps as f64 * 1000.0 / 8.0),
    };
    let block_bytes: u64 = args.get("--block-bytes", 13_000)?;
    let budget_mb: u32 = args.get("--budget-mb", 8)?;
    let idle_rounds: usize = args.get("--idle-rounds", 5)?;
    let max_secs = Duration::from_secs(args.get("--max-secs", 120)?);
    let seed: u64 = args.get("--seed", 1)?;
    if tenant_count < group_count {
        return Err("--tenants must be at least --groups".into());
    }

    // The chain: group 0 at the high-water mark, the others spaced below
    // it, and `backlog` blocks above it to the node's tip. Block times end
    // now, so every block falls inside the orders' windows.
    let spacing = backlog / group_count as u64;
    let lowest = 10;
    let high_water = lowest + spacing * (group_count as u64 - 1);
    let tip = high_water + backlog;
    let now = engine::now_unix();
    let tip_time = u64::try_from(now)?;
    let first_block_time = now - ((tip - lowest) * BLOCK_SECS) as i64;

    let store = Store::open_file(&db_path)?;
    let sql = rusqlite::Connection::open(&db_path)?;
    sql.execute_batch("PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;")?;
    let custody: Arc<dyn KeyCustody> = Arc::new(PlainKeyCustody::default());
    let mut handles: HashMap<TenantId, WalletHandle> = HashMap::with_capacity(tenant_count);
    let mut groups: Vec<Group> = (0..group_count)
        .map(|g| Group {
            start: high_water - spacing * g as u64,
            tenants: Vec::new(),
        })
        .collect();
    for index in 0..tenant_count {
        let keys = stress_common::material(seed, index)?;
        let handle = custody.register_wallet(keys.clone()).await?;
        let address = custody
            .derive_subaddress(handle, SubaddressIndex::default(), Network::Mainnet)
            .await?;
        let created = store.create_tenant(
            NewTenant {
                key_custody_backend: "plain".into(),
                sealed_key_material: custody.seal(&keys).await?,
                primary_address: address.to_string(),
                network: NETWORK.into(),
                confirmations_required: Some(10),
                order_expiry_seconds: Some(3600),
            },
            first_block_time - 7200,
        )?;
        let group = index % group_count;
        let id = TenantId::new(format!("tn_sweep_{group:04}_{index:08}"));
        sql.execute(
            "UPDATE tenants SET id = ?1, scanned_through_height = ?2 WHERE id = ?3",
            rusqlite::params![&id, groups[group].start as i64, &created.tenant.id],
        )?;
        // Two open orders, open since before the first block, so every
        // tenant is scanned for every block (tenant 0's first one is paid
        // in each).
        for _ in 0..2 {
            let minor = store.allocate_minor_index(&shared::ids::TenantId::new(id.to_string()))?;
            let subaddress = custody
                .derive_subaddress(
                    handle,
                    SubaddressIndex { major: 0, minor },
                    Network::Mainnet,
                )
                .await?;
            store.create_order(NewOrder {
                idempotency_key: None,
                confirmations_required_override: None,
                tenant_id: id.clone(),
                merchant_order_id: None,
                minor_index: minor,
                address: subaddress.to_string(),
                xmr_amount_piconero: 1,
                description: None,
                created_at: first_block_time - 7200,
                expires_at: now + 7 * 24 * 3600,
            })?;
        }
        groups[group].tenants.push(id.clone());
        handles.insert(id, handle);
    }
    // The recorded chain, from the lowest group up to the high-water mark.
    for height in lowest..=high_water {
        store.set_scanned_block(Network::Mainnet, height, &SweepDaemon::hash(height))?;
    }
    drop(sql);

    let full_tx = stress_common::fixture_tx()?;
    let daemon = Arc::new(SweepDaemon {
        tip,
        tip_time,
        block_bytes,
        tx: ScanTx::of(&full_tx),
        txid: stress_common::txid(&full_tx),
        full_tx,
        link,
        blocks_served: AtomicU64::new(0),
        bytes_served: AtomicU64::new(0),
        block_requests: AtomicU64::new(0),
        small_requests: AtomicU64::new(0),
        heights_served: Mutex::new(HashSet::new()),
    });
    let db = Db::open(&db_path, &store)?;
    let observer = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    observer.busy_timeout(Duration::from_secs(5))?;
    let tenants: Vec<(TenantId, WalletHandle)> =
        handles.iter().map(|(id, h)| (id.clone(), *h)).collect();
    let progress = engine::scaling::new_progress();
    let state = ScanState::default().with_progress(progress.clone());
    let inputs = RoundInputs {
        db: &db,
        custody: custody.as_ref(),
        daemon: daemon.as_ref(),
        network: Network::Mainnet,
        tenants: &tenants,
        reorg_check_depth: 10,
        grace_period_seconds: 3600,
        scan_chunk_memory_budget_mb: budget_mb,
    };

    // Catch up: rounds back to back until every group reaches the tip, or
    // the time runs out.
    let needed: u64 = groups.iter().map(|g| tip - g.start).sum();
    let mut round_ms = Vec::new();
    let mut steps: HashMap<Tier, u64> = HashMap::new();
    let mut errors = 0u64;
    let started = Instant::now();
    let drained = loop {
        let round_started = Instant::now();
        let report = engine::work::run_round(&state, &inputs, round_budget).await;
        round_ms.push(round_started.elapsed().as_millis() as u64);
        for (tier, count) in report.steps.iter() {
            *steps.entry(tier).or_default() += u64::from(count);
        }
        errors += u64::from(report.error.is_some());
        if group_cursors(&observer, &groups)?.iter().all(|c| *c >= tip) {
            break true;
        }
        if started.elapsed() >= max_secs {
            break false;
        }
    };
    let catch_up = started.elapsed();
    let scanned: u64 = group_cursors(&observer, &groups)?
        .iter()
        .zip(&groups)
        .map(|(cursor, group)| cursor.saturating_sub(group.start))
        .sum();

    // With nothing left to scan: what a round costs by itself.
    let mut idle_ms = Vec::new();
    for _ in 0..idle_rounds {
        let round_started = Instant::now();
        let _ = engine::work::run_round(&state, &inputs, round_budget).await;
        idle_ms.push(round_started.elapsed().as_micros() as u64);
    }

    let mut sorted = round_ms.clone();
    sorted.sort_unstable();
    idle_ms.sort_unstable();
    let blocks_served = daemon.blocks_served.load(Ordering::Relaxed);
    let distinct = daemon.heights_served.lock().len() as u64;
    let result = json!({
        "schema_version": SCHEMA_VERSION,
        "tenants": tenant_count,
        "groups": group_count,
        "backlog_blocks": backlog,
        "round_budget_ms": round_budget.as_millis() as u64,
        "link_kbps": link_kbps,
        "rtt_ms": link.rtt.as_millis() as u64,
        "ttfb_us_per_block": link.ttfb_per_block.as_micros() as u64,
        "block_bytes": block_bytes,
        "budget_mb": budget_mb,
        "seed": seed,
        "drained": drained,
        "catch_up_ms": catch_up.as_millis() as u64,
        "blocks_needed": needed,
        "blocks_scanned": scanned,
        "blocks_per_sec": scanned as f64 / catch_up.as_secs_f64().max(1e-9),
        "blocks_served": blocks_served,
        "distinct_heights_served": distinct,
        "bytes_served": daemon.bytes_served.load(Ordering::Relaxed),
        "discarded_cache_bytes": progress.lock().discarded_cache_bytes,
        "served_per_scanned": blocks_served as f64 / scanned.max(1) as f64,
        "block_requests": daemon.block_requests.load(Ordering::Relaxed),
        "small_requests": daemon.small_requests.load(Ordering::Relaxed),
        "rounds": round_ms.len(),
        "round_errors": errors,
        "round_ms_p50": quantile(&sorted, 0.5),
        "round_ms_max": sorted.last().copied().unwrap_or(0),
        "idle_round_us_p50": quantile(&idle_ms, 0.5),
        "steps": steps.iter().map(|(tier, n)| (tier.to_string(), *n)).collect::<HashMap<_, _>>(),
    });
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

fn main() {
    // Two workers, as `server.worker_threads` defaults to.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build();
    let result = match runtime {
        Ok(runtime) => runtime.block_on(sweep()),
        Err(error) => Err(error.into()),
    };
    if let Err(error) = result {
        eprintln!("round sweep: {error}");
        std::process::exit(1);
    }
}
