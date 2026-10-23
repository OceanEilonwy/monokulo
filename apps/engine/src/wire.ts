// What monokulo sends the engine page (crates/monokulo/src/http/engine_page.rs),
// as its JSON is written: the state machine's state and marks
// (engine_view::machine), the frames around them (engine_view::history), and
// the figures behind the "Machine and links" strip (the `machine` event).
// Field names and shapes follow the Rust types' serde forms; the app writes
// every word from these (present.ts, machine.ts).

export type Tier = 'chain' | 'blocks' | 'mempool' | 'settlement' | 'upkeep';
/** In the order the round runs them (`Tier::ALL`). */
export const TIERS: readonly Tier[] = ['chain', 'blocks', 'mempool', 'settlement', 'upkeep'];

export type Wait =
  | 'chain_height_unknown'
  | 'reorg_being_reconciled'
  | 'rewound_this_round'
  | 'node_failed'
  | 'node_cannot_serve_tip'
  | 'mempool_unreadable'
  | 'reorg_candidates_retrying'
  | 'chain_diverged';
export type TierOutcome = 'idle' | 'backlogged' | 'failed' | { blocked: Wait };
export type UnitProgress = 'advanced' | 'idle' | 'failed' | { blocked: Wait };
export type Group = 'frontier' | 'catch_up';
export type Wake = 'interval' | 'new_block';
export type PoolPath = 'fast' | 'round';
export type Work = 'tip_request' | 'cache_carry';
export type OrderStatus = 'pending' | 'unconfirmed' | 'confirming' | 'paid' | 'partial' | 'overpaid' | 'expired' | 'cancelled';

/** Everything the page shows about one network at one moment. */
export interface State {
  at_ms: number;
  chain: Chain;
  /** Highest first. */
  groups: StoreGroup[];
  more_groups: number;
  round: Round | null;
  /** Oldest first. */
  ribbon: RibbonEntry[];
  chain_agrees: boolean | null;
  reorg: Reorg | null;
  pool: Pool;
  node_pool: NodePool | null;
  orders: Orders;
  upkeep: Upkeep;
  database: Database;
  nodes: Node[];
  last_call: Call | null;
  saves: PerMinute;
  saved: Saved;
}

export interface Chain {
  tip: number | null;
  high_water: number | null;
  cached: number[];
  cache_bytes: number;
  cache_budget_bytes: number;
  /** By height: transactions done of the total. */
  checkpoints: Record<string, [number, number]>;
  replaced: number[];
  scanning: Scanning | null;
}

export interface Scanning {
  height: number;
  group: Group;
  stores: number;
  done_txs: number;
  total_txs: number;
  header_only: boolean;
}

export interface StoreGroup {
  id: number;
  cursor: number;
  stores: number;
}

export interface Round {
  number: number;
  budget_ms: number;
  tip: number | null;
  woken_by: Wake | null;
  to_budget: boolean;
  units: UnitBar[];
  /** In tier order. */
  ended: (TierOutcome | null)[];
  elapsed_ms: number;
  finished: { ms: number; backlogged: boolean } | null;
}

export interface UnitBar {
  tier: Tier;
  start_ms: number;
  ms: number;
  span: Span;
}

export type Span = { kind: 'unit'; pass: number; progress: UnitProgress } | { kind: 'work'; what: Work };

export type RibbonEntry =
  | { kind: 'round'; number: number; ms: number; tiers_ms: number[]; backlogged: boolean }
  | { kind: 'sleep'; ms: number; woken_by: Wake };

export interface Reorg {
  fork: number;
  step: 'found' | 'collecting' | 'processing';
  candidates: number | null;
  examined: number;
  changed: number;
  voided: number;
}

export interface Pool {
  watched: boolean;
  remembered: number;
  txs: { txid: string; matched: boolean }[];
  fast_passes: PerMinute;
  found: number;
}

/** Counts over the last minute, a count a second: `[second, how many]`. */
export interface PerMinute {
  seconds: [number, number][];
}

export interface NodePool {
  txs: number;
  bytes: number | null;
  penalty_free: number | null;
}

export interface Orders {
  pending: number;
  due: number;
  /** Newest first. */
  last: { from: OrderStatus; to: OrderStatus }[];
  looked: number;
}

export interface Upkeep {
  round: number | null;
  pruned: number;
}

export interface Database {
  queued: [number, number];
  capacity: number;
  completed: number;
  max_queue_wait_us: number;
  max_run_us: number;
}

export interface Node {
  label: string;
  active: boolean;
  cooling_down: boolean;
}

export type Call =
  | { kind: 'block_hash'; height: number }
  | { kind: 'blocks'; from: number; count: number }
  | { kind: 'pool' }
  | { kind: 'transactions' };

export interface Saved {
  cursors: number;
  checkpoints: number;
  reorg: number;
  payments: number;
  recomputes: number;
}

/** What happened, as data: a line on the timeline, a row in the events table. */
export interface Mark {
  seq: number;
  at_ms: number;
  round: number;
  tier: Tier;
  key: boolean;
  what: MarkKind;
  /** When it reached the page (`performance.now()`); absent for the history. */
  arrived?: number;
}

export type CommitHow = 'header_only' | 'frontier' | 'joined' | 'catching_up';

export type MarkKind =
  | { kind: 'new_blocks'; tip: number; count: number }
  | { kind: 'reorg_found'; fork: number; deeper: boolean }
  | { kind: 'reorg_collected' }
  | { kind: 'reorg_processed'; examined: number; changed: number; voided: number }
  | { kind: 'rewound'; fork: number; deleted_to: number | null; ancestor: number }
  | { kind: 'seeded'; height: number }
  | { kind: 'checkpointed'; height: number; stores: number; done_txs: number; total_txs: number }
  | { kind: 'committed'; height: number; stores: number; found: number; how: CommitHow }
  | { kind: 'diverged'; height: number }
  | { kind: 'idle_advanced'; from: number; to: number; stores: number }
  | { kind: 'fast_scanned'; transactions: number }
  | { kind: 'tx_matched'; path: PoolPath; txid: string }
  | { kind: 'recomputed'; orders: number; from: OrderStatus; to: OrderStatus }
  | { kind: 'pruned'; hashes: number }
  | { kind: 'tier_failed'; tier: Tier }
  | { kind: 'node_failed'; tier: Tier };

/** A place on the page a movement starts or ends at. */
export type Anchor =
  | { kind: 'node' | 'pool' | 'reorg' | 'orders' | 'database' | 'upkeep' }
  | { kind: 'cell' | 'group'; id: number };

/** A movement leading to the next state. */
export type Effect =
  | { kind: 'packet'; to: Anchor; call: Call }
  | { kind: 'fly'; from: Anchor; to: Anchor; token: 'payment' | 'stores' }
  | { kind: 'save'; at: Anchor }
  | { kind: 'flash'; at: Anchor }
  | { kind: 'probe'; height: number }
  | { kind: 'new_blocks'; from: number; to: number }
  | { kind: 'drop'; from: number; to: number };

export interface Timed {
  at_ms: number;
  effect: Effect;
}

/** How the scanner's rounds are sized: what a round is drawn to. */
export interface Tuning {
  round_ms: number;
  /** Percent, in tier order. */
  shares: number[];
  group_page: number;
  blocks_per_unit: number;
  reorg_check_depth: number;
  poll_ms: number;
}

/** The state at one moment, and what led there. */
export interface Frame {
  at_ms: number;
  state: State;
  tuning: Tuning;
  effects: Timed[];
  marks: Mark[];
}

/** The `history` event: what the app starts from. */
export interface Start {
  epoch: string;
  engine_now_ms: number;
  oldest_ms: number | null;
  frame: Frame;
  marks: Mark[];
}

// ---- the `machine` event ----

export interface Machine {
  network: string;
  /** When the engine wrote the `/status` this is from (its clock). */
  at_unix: number;
  /** Now, by monokulo's clock: where the charts' hour ends. */
  now_unix: number;
  resources: Resources;
  /** The node the engine reads the network's chain from right now. */
  node: NodeInUse | null;
  scaling: NetworkScaling | null;
}

export interface Resources {
  engine: ResourceReport | null;
  monokulo: ResourceReport;
  /** The engine runs inside monokulo: one process, memory shared. */
  one_process: boolean;
}

export interface ResourceReport {
  host_id: string;
  cpu_count: number;
  machine_memory_bytes: number | null;
  cgroup_memory_bytes: number | null;
  samples: ResourceSample[];
}

export interface ResourceSample {
  unix: number;
  cpu_percent: number;
  memory_bytes: number;
  hosted_cpu_percent?: number | null;
}

export interface NodeInUse {
  label: string;
  fallback: boolean;
  link: LinkSnapshot | null;
}

export interface LinkSnapshot {
  measured: boolean;
  rtt_ms: number;
  ttfb_per_block_ms: number;
  rate_bytes_per_sec: number;
  bytes_per_block: number;
  last_measured_unix: number | null;
  timeouts_last_hour: number;
  failures_last_hour: number;
  history: LinkPoint[];
}

export interface LinkPoint {
  minute_unix: number;
  rate_bytes_per_sec: number;
  rtt_ms: number;
  ttfb_per_block_ms: number;
}

export type Trend = 'rising' | 'falling' | 'steady';
export type Pace = 'caught_up' | 'memory' | 'link' | 'cpu' | 'round';
export type ChunkLimit = 'memory' | 'link' | 'maximum' | 'remaining' | 'cpu';

export interface NetworkScaling {
  scan: ScanReport;
  blocks_behind: number;
  catch_up_secs: number | null;
  pace: Pace;
  budget_mb: number;
  max_budget_mb: number | null;
  round_deadline_secs: number;
  round_base_secs: number;
  slow: SlowBlock | null;
}

export interface ScanReport {
  avg_block_bytes: number;
  block_size_trend: Trend;
  last_chunk: { blocks: number; limited_by: ChunkLimit } | null;
  blocks_per_minute: number;
  fetch_secs_recent: number;
  scan_secs_recent: number;
  largest_recent: { height: number; wire_bytes: number; secs: number; finished_unix: number } | null;
  in_progress: {
    height: number;
    started_unix: number;
    wire_bytes: number | null;
    pages?: { done_txs: number; total_txs: number; page_txs: number } | null;
  } | null;
  in_progress_secs: number | null;
  peak_cache_bytes: number | null;
  discarded_cache_bytes_recent?: number;
  round_budget_secs?: number | null;
  headers_first?: { remaining_secs: number; reason: 'failed_request' | 'large_block' } | null;
}

export interface SlowBlock {
  height: number;
  wire_bytes: number | null;
  elapsed_secs: number;
  node: string | null;
  rate_bytes_per_sec: number | null;
  remaining_secs: number | null;
}
