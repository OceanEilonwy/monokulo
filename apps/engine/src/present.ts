// The engine page's words and figures for one state: everything a person
// reads on the page is written here, from the state machine's state and
// marks as monokulo sends them (wire.ts). The views draw a `Presented`;
// none formats a number or writes a sentence of its own.

import {
  DASH, capitalised, compact, fraction, kilobytes, laneTime, megabytes, micros, plural, rounded, seconds,
  storesPhrase, thousands,
} from './format.ts';
import type {
  MarkKind, NodePool, OrderStatus, PerMinute, RibbonEntry, Round, State, Tier, TierOutcome, Tuning, Wait,
} from './wire.ts';
import { TIERS } from './wire.ts';

export interface Presented {
  summary: Figure[];
  chain: ChainView;
  round: RoundView | null;
  ribbon: RibbonMark[];
  side: Side;
}

/** One figure of the summary row. */
export interface Figure {
  label: string;
  value: string;
  note: string;
}

export interface ChainView {
  tip: number | null;
  high_water: number | null;
  /** The lowest block a group of stores has reached. */
  lowest: number | null;
  /** Where the reorg window (blocks checked again every round) starts. */
  window_from: number | null;
  cached: number[];
  /** Blocks with a scan saved partway, and how far (0 to 1). */
  checkpoints: [number, number][];
  replaced: number[];
  /** The block being scanned, and how far (0 to 1). */
  scanning: [number, number] | null;
  groups: GroupView[];
  /** The block still to come, filled by the node's pool. */
  next_block: NextBlock | null;
  cache: string;
  nodes: NodeView[];
  /** The last call to the node in use, as monerod names it. */
  call: string | null;
}

export interface GroupView {
  id: number;
  cursor: number;
  frontier: boolean;
  /** Being scanned for now. */
  busy: boolean;
  /** Waiting (for a reorg to be reconciled). */
  waiting: boolean;
  label: string;
  title: string;
}

export interface NextBlock {
  /** How full, of the size a miner can fill at full reward (0 to 1), when
   *  the node said how big its pool is. */
  fill: number | null;
  /** More than a block takes at full reward. */
  over: boolean;
  /** The pool's transactions, short: "23", "1.2k". */
  count: string;
  title: string;
}

export interface NodeView {
  label: string;
  /** The node the engine is using now: the last call goes under it. */
  active: boolean;
  chip: string;
  /** `ok` for the active node, empty for a fallback, `warn` cooling down. */
  tone: string;
}

export interface RoundView {
  number: number;
  title: string;
  state: string;
  /** The length the lanes are drawn to. */
  scale_ms: number;
  /** How far the round has got: the sum of its lanes' times. */
  elapsed_ms: number;
  elapsed: string;
  lanes: Lane[];
}

export interface Lane {
  tier: Tier;
  name: string;
  share: string;
  /** The tier's time in the round, everything it waited on included. */
  ms: number;
  time: string;
  bars: Bar[];
  /** The tier's reserved share, from where it started: drawn while the
   *  round is drawn to its budget. */
  reserved: [number, number] | null;
  outcome: Chip | null;
}

/** A segment of a lane: a run of the tier's units, drawn solid, with the
 *  tier's work outside its units drawn outlined over only the time it took
 *  (docs/engine_visualizer_decisions.md D39). */
export interface Bar {
  start_ms: number;
  /** The time it took: what its label and the round's total add up. */
  ms: number;
  /** How long it is drawn, from its start to its last piece's end. */
  span_ms: number;
  /** What it is drawn as, left to right. */
  shapes: Shape[];
  /** Only work for the tier outside its units. */
  work: boolean;
  title: string;
  /** Aggregated operations, for the timing details. */
  details: string[];
  /** Its time, written after it; null when the next segment of the lane is
   *  too close, which then carries this one's time too. */
  label: string | null;
  /** The segment that finished last: the round's end marker is just past
   *  its right edge. */
  last: boolean;
}

/** One drawn piece of a segment, in the round's milliseconds. */
export type Shape =
  /** The tier's units: solid; `leftover` ran in pass 2 (striped). */
  | { kind: 'fill'; from_ms: number; to_ms: number; leftover: boolean }
  /** The tier's work outside its units: an outline. `joined`: it starts
   *  where the shape before it ends. `band_from`: split on a wide track,
   *  joined from there on a narrow one. */
  | { kind: 'work'; from_ms: number; to_ms: number; joined: boolean; band_from: number | null }
  /** A 1px line across the time other tiers ran between a unit and the
   *  tier's later work. */
  | { kind: 'thread'; from_ms: number; to_ms: number; band: boolean };

/** Whether `next`, the shape after `shape` in its segment, starts where it
 *  ends: the two are drawn as one run, square where they meet. A thread is
 *  a line, not a box: nothing meets it. */
export function meets(shape: Shape | undefined, next: Shape | undefined): boolean {
  return !!shape && !!next && shape.kind !== 'thread' && next.kind !== 'thread' && shape.to_ms === next.from_ms;
}

export interface Chip {
  /** `ok`, `hi`, `warn` or `err`: the theme's chip tones. */
  tone: string;
  text: string;
}

export type RibbonMark =
  | {
    kind: 'round';
    number: number;
    /** Height in pixels, by the round's length on a log scale. */
    height: number;
    /** Each tier's part of the round, in tier order. */
    parts: [Tier, number][];
    title: string;
  }
  | { kind: 'sleep'; woken: boolean; title: string };

/** The panels' one-line summaries, and their details. */
export interface Side {
  reorg: Panel;
  mempool: Panel;
  orders: Panel;
  upkeep: Panel;
  database: Panel;
  restart: Panel;
  reorg_step: 'found' | 'collect' | 'process' | null;
  pool_txs: [string, boolean][];
  /** The last status changes, newest first. */
  transitions: [OrderStatus, OrderStatus][];
  queues: [number, number];
  queue_capacity: number;
}

export interface Panel {
  summary: string;
  /** Draws attention: open by itself, with an alert edge. */
  alert: boolean;
  rows: [string, string][];
}

/** A round shorter than this is drawn to this scale. */
const MIN_SCALE_MS = 10;
/** A gap between a segment's pieces narrower than this share of the drawn
 *  scale is covered by the outline after it (joined); a wider one is
 *  crossed by a thread. */
const JOIN_SHARE = 0.025;
/** On a narrow track the same share is far fewer pixels: gaps under this
 *  are joined there too (the page's container query, `.band`). */
const JOIN_SHARE_NARROW = 0.05;
/** Segments closer than this, as a share of the drawn length, share one
 *  label. */
const LABEL_GAP = 0.08;

const TIER_NAMES: Record<Tier, string> = {
  chain: 'Chain', blocks: 'Blocks', mempool: 'Mempool', settlement: 'Settlement', upkeep: 'Upkeep',
};
export const tierName = (tier: Tier): string => TIER_NAMES[tier];

const count = (perMinute: PerMinute): number => perMinute.seconds.reduce((sum, [, n]) => sum + n, 0);
const sum = (values: number[]): number => values.reduce((total, n) => total + n, 0);

/** What happened, as a sentence. */
export function markText(what: MarkKind): string {
  switch (what.kind) {
    case 'new_blocks':
      return what.count === 1
        ? `The node has a new block: ${thousands(what.tip)}.`
        : `The node has ${thousands(what.count)} new blocks, up to ${thousands(what.tip)}.`;
    case 'reorg_found':
      return what.deeper
        ? `The reorganisation goes deeper: it is reconciled from block ${thousands(what.fork)} now.`
        : `The node's chain differs from block ${thousands(what.fork)} on: a reorganisation. Payments from there are re-examined, and new blocks wait.`;
    case 'reorg_collected':
      return 'Payments at or above the fork were queued to be re-examined.';
    case 'reorg_processed':
      return `Re-examined ${plural(what.examined, 'payment')} against the node's chain: ${thousands(what.changed)} changed, ${thousands(what.voided)} voided as double-spent.`;
    case 'rewound':
      return what.deleted_to !== null && what.deleted_to > what.fork
        ? `Rewound: blocks ${thousands(what.fork)} to ${thousands(what.deleted_to)} deleted, stores moved back to block ${thousands(what.ancestor)}. The replacement blocks are scanned next.`
        : `Rewound: block ${thousands(what.fork)} deleted, stores moved back to block ${thousands(what.ancestor)}. The replacement block is scanned next.`;
    case 'seeded':
      return `Started scanning this network at block ${thousands(what.height)}.`;
    case 'checkpointed':
      return `Out of time partway through block ${thousands(what.height)}: ${thousands(what.done_txs)} of ${thousands(what.total_txs)} transactions scanned for ${storesPhrase(what.stores)}, saved.`;
    case 'committed': {
      const found = what.found > 0 ? `, ${plural(what.found, 'payment')} found in it` : '';
      const [height, stores] = [thousands(what.height), storesPhrase(what.stores)];
      switch (what.how) {
        case 'header_only':
          return `Block ${height} recorded from its header: no store had anything to look for in it.`;
        case 'frontier':
          return `Block ${height} scanned for ${stores} and committed${found}.`;
        case 'joined':
          return `Block ${height} scanned for ${stores} catching up${found}: they caught up and joined the frontier.`;
        case 'catching_up':
          return `Block ${height} scanned for ${stores} catching up${found}.`;
      }
      break;
    }
    case 'diverged':
      return `Block ${thousands(what.height)} from the node doesn't extend the recorded chain: new blocks wait for the reorganisation to be reconciled.`;
    case 'idle_advanced':
      return `${capitalised(storesPhrase(what.stores))} with nothing that could have been paid moved straight from block ${thousands(what.from)} to ${thousands(what.to)}.`;
    case 'fast_scanned':
      return `The fast path scanned ${plural(what.transactions, 'transaction')} new in the pool.`;
    case 'tx_matched':
      return what.path === 'fast'
        ? `Transaction ${what.txid} in the pool pays an order: recorded at once by the fast path.`
        : `Transaction ${what.txid} in the pool pays an order: found by the round's rotation.`;
    case 'recomputed':
      return what.orders === 1
        ? `An order went from ${what.from} to ${what.to}; its order event is written.`
        : `${thousands(what.orders)} orders changed status (one from ${what.from} to ${what.to}); their order events are written.`;
    case 'pruned':
      return `Pruned ${thousands(what.hashes)} old block hashes.`;
    case 'tier_failed':
      return `The ${what.tier} tier failed and stopped for this round; it is retried next round.`;
    case 'node_failed':
      return `The ${what.tier} tier stopped for this round: the node failed.`;
  }
  throw new Error(`no sentence for ${JSON.stringify(what satisfies never)}`);
}

/** `state` in words and figures, its rounds to `tuning`'s scale. */
export function present(state: State, tuning: Tuning): Presented {
  return {
    summary: summary(state),
    chain: chain(state, tuning),
    round: presentRound(state, tuning),
    ribbon: state.ribbon.map(ribbonMark),
    side: side(state),
  };
}

/** The stores behind the high-water mark. */
function catchingUp(state: State): number {
  const high = state.chain.high_water;
  if (high === null) return 0;
  return sum(state.groups.filter((group) => group.cursor < high).map((group) => group.stores));
}

function minimum(values: number[]): number | null {
  return values.length ? Math.min(...values) : null;
}

function summary(state: State): Figure[] {
  const chain = state.chain;
  const lowest = minimum(state.groups.map((group) => group.cursor));
  const low = lowest ?? chain.high_water;
  const behind = chain.tip !== null && low !== null ? Math.max(0, chain.tip - low) : null;
  const catching = catchingUp(state);
  const stores = sum(state.groups.map((group) => group.stores));
  const lastRound = [...state.ribbon].reverse().find((entry) => entry.kind === 'round');
  const active = state.nodes.find((node) => node.active);
  return [
    {
      label: 'Node tip',
      value: chain.tip === null ? DASH : thousands(chain.tip),
      note: active ? active.label : 'no node reported yet',
    },
    {
      label: 'Scanned to',
      value: chain.high_water === null ? DASH : thousands(chain.high_water),
      note: 'high-water mark',
    },
    {
      label: 'Behind',
      value: behind === null ? DASH : thousands(behind),
      note: behind === null ? 'not known yet'
        : behind === 0 ? 'caught up'
          : catching > 0 ? `${storesPhrase(catching)} catching up`
            : 'new blocks to scan',
    },
    {
      label: 'Stores',
      value: thousands(stores),
      note: catching > 0 ? `${thousands(catching)} catching up` : 'all at the high-water mark',
    },
    {
      label: 'Last round',
      value: lastRound ? seconds(lastRound.ms) : DASH,
      note: lastRound && lastRound.kind === 'round' && lastRound.backlogged ? 'work was left' : 'of a 10s budget',
    },
    {
      label: 'Chain',
      value: state.reorg ? 'Reorg'
        : chain.replaced.length ? 'Diverged'
          : state.chain_agrees === true ? 'Agrees'
            : state.chain_agrees === false ? 'Differs'
              : DASH,
      note: state.reorg ? `from block ${thousands(state.reorg.fork)}`
        : chain.replaced.length ? 'waiting for reconciliation'
          : 'with the node',
    },
  ];
}

function chain(state: State, tuning: Tuning): ChainView {
  const chain = state.chain;
  const high = chain.high_water;
  return {
    tip: chain.tip,
    high_water: high,
    lowest: minimum(state.groups.map((group) => group.cursor)),
    window_from: high === null ? null : Math.max(0, high - Math.max(0, tuning.reorg_check_depth - 1)),
    cached: [...chain.cached],
    checkpoints: Object.entries(chain.checkpoints)
      .map(([height, [done, total]]): [number, number] => [Number(height), fraction(done, total)])
      .sort((a, b) => a[0] - b[0]),
    replaced: [...chain.replaced],
    scanning: chain.scanning ? [chain.scanning.height, fraction(chain.scanning.done_txs, chain.scanning.total_txs)] : null,
    groups: state.groups.map((group) => {
      const frontier = group.cursor === high;
      const scanning = chain.scanning;
      const busy = !!scanning && scanning.height === group.cursor + 1 && (scanning.group === 'frontier') === frontier;
      const pages = group.stores > tuning.group_page
        ? ` (${Math.ceil(group.stores / Math.max(1, tuning.group_page))} pages)`
        : '';
      return {
        id: group.id,
        cursor: group.cursor,
        frontier,
        busy,
        waiting: state.reorg !== null,
        label: `${frontier ? 'Frontier' : 'Catching up'}, ${storesPhrase(group.stores)}${pages}`,
        title: `${storesPhrase(group.stores)} whose scan has reached block ${thousands(group.cursor)}`,
      };
    }),
    next_block: state.node_pool ? nextBlock(state.node_pool) : null,
    cache: `cache ${megabytes(chain.cache_bytes)} of ${megabytes(chain.cache_budget_bytes)}`,
    nodes: state.nodes.map((node) => ({
      label: node.label,
      active: node.active,
      chip: node.active ? 'active' : node.cooling_down ? 'cooling down' : 'fallback',
      tone: node.active ? 'ok' : node.cooling_down ? 'warn' : '',
    })),
    call: callName(state),
  };
}

function callName(state: State): string | null {
  const call = state.last_call;
  if (!call) return null;
  switch (call.kind) {
    case 'block_hash': return `on_get_block_hash ${thousands(call.height)}`;
    case 'blocks': return `get_blocks.bin ${thousands(call.count)} from ${thousands(call.from)}`;
    case 'pool': return 'get_transaction_pool_hashes';
    case 'transactions': return 'get_transactions';
  }
}

function nextBlock(pool: NodePool): NextBlock {
  const known = pool.bytes !== null && pool.penalty_free !== null;
  const fill = known ? pool.bytes! / Math.max(1, pool.penalty_free!) : null;
  const over = fill !== null && fill > 1;
  const transactions = plural(pool.txs, 'transaction');
  return {
    fill: fill === null ? null : rounded(Math.min(1, fill)),
    over,
    count: compact(pool.txs),
    title: known
      ? `The next block: ${transactions} waiting in the node's pool, ${kilobytes(pool.bytes!)} of the ${kilobytes(pool.penalty_free!)} a miner can fill at full reward (${((fill ?? 0) * 100).toFixed(0)} %)${over ? ': more than one block takes without a smaller reward.' : '.'}`
      : `The next block: ${transactions} waiting in the node's pool (the node didn't say their size).`,
  };
}

/** The round in progress, or the last one, as the round card draws it. */
export function presentRound(state: State, tuning: Tuning): RoundView | null {
  return state.round ? roundView(state, state.round, tuning) : null;
}

/** One of a tier's spans, as a segment is built from them. */
export interface Piece {
  start_ms: number;
  ms: number;
  solid: boolean;
  leftover: boolean;
}

/** What `pieces` (one segment's, in time order) are drawn as at `scale`. */
export function shapes(pieces: Piece[], scale: number): Shape[] {
  const out: Shape[] = [];
  let end: number | null = null;
  const share = (gap: number) => gap / Math.max(1, scale);
  for (const piece of pieces) {
    const to = piece.start_ms + piece.ms;
    const gap: number | null = end === null ? null : Math.max(0, piece.start_ms - end);
    const last = out[out.length - 1];
    if (piece.solid) {
      // A unit is never after a gap in one segment: a unit after a gap
      // starts the next segment.
      if (last && last.kind === 'fill' && gap === 0 && last.leftover === piece.leftover) last.to_ms = to;
      else out.push({ kind: 'fill', from_ms: piece.start_ms, to_ms: to, leftover: piece.leftover });
    } else if (gap === null) {
      out.push({ kind: 'work', from_ms: piece.start_ms, to_ms: to, joined: false, band_from: null });
    } else {
      const before = end ?? piece.start_ms;
      if (share(gap) < JOIN_SHARE) {
        // Joined to an outline: the same box, on to here.
        if (last && last.kind === 'work') last.to_ms = to;
        else out.push({ kind: 'work', from_ms: before, to_ms: to, joined: true, band_from: null });
      } else {
        const band = share(gap) < JOIN_SHARE_NARROW;
        out.push({ kind: 'thread', from_ms: before, to_ms: piece.start_ms, band });
        out.push({ kind: 'work', from_ms: piece.start_ms, to_ms: to, joined: false, band_from: band ? before : null });
      }
    }
    end = end === null ? to : Math.max(end, to);
  }
  return out;
}

const operation = (tier: Tier, unit: Round['units'][number]): string => {
  const span = unit.span;
  if (span.kind === 'work') return span.what === 'tip_request' ? 'Node tip requests' : 'Fetched block cache updates';
  if (span.pass === 2) return 'Work using remaining budget';
  switch (tier) {
    case 'chain': return 'Chain checks';
    case 'blocks': return 'Block scan work';
    case 'mempool': return 'Mempool scan work';
    case 'settlement': return 'Order settlement work';
    case 'upkeep': return 'Housekeeping work';
  }
};

const isLeftover = (unit: Round['units'][number]) => unit.span.kind === 'unit' && unit.span.pass === 2;
const spaced = (ms: number) => laneTime(ms).replace('ms', ' ms');

/** `tier`'s segments in `round`: a run of the tier's units with its later
 *  work outside its units; a unit after a gap starts the next one. One that
 *  took no time is left out where the lane has one that took some. */
function segments(round: Round, tier: Tier, scale: number): Bar[] {
  // Each segment's pieces, and its operations' counts and times by name
  // (in name order, as they are listed).
  let built: { pieces: Piece[]; parts: Map<string, [number, number]> }[] = [];
  for (const unit of round.units.filter((unit) => unit.tier === tier)) {
    const name = operation(tier, unit);
    const piece: Piece = { start_ms: unit.start_ms, ms: unit.ms, solid: unit.span.kind !== 'work', leftover: isLeftover(unit) };
    const current = built[built.length - 1];
    const attaches = !!current && (() => {
      const end = Math.max(0, ...current.pieces.map((p) => p.start_ms + p.ms));
      return !piece.solid || piece.start_ms === end;
    })();
    if (attaches) {
      current.pieces.push(piece);
      const entry = current.parts.get(name) ?? [0, 0];
      current.parts.set(name, [entry[0] + 1, entry[1] + unit.ms]);
    } else {
      built.push({ pieces: [piece], parts: new Map([[name, [1, unit.ms]]]) });
    }
  }
  const took = (pieces: Piece[]) => sum(pieces.map((p) => p.ms));
  if (built.some(({ pieces }) => took(pieces) > 0)) built = built.filter(({ pieces }) => took(pieces) > 0);
  else built = built.slice(0, 1);
  const close = Math.trunc(scale * LABEL_GAP);
  let carried = 0;
  return built.map(({ pieces, parts }, i) => {
    const nextStart = built[i + 1]?.pieces[0]?.start_ms ?? null;
    const start = pieces[0]?.start_ms ?? 0;
    const end = pieces.length ? Math.max(...pieces.map((p) => p.start_ms + p.ms)) : start;
    const ms = took(pieces);
    const names = [...parts.keys()].sort();
    const operations = sum(names.map((name) => parts.get(name)![0]));
    const bar: Bar = {
      start_ms: start,
      ms,
      span_ms: end - start,
      shapes: shapes(pieces, scale),
      work: pieces.every((p) => !p.solid),
      title: `${tierName(tier)}: ${spaced(ms)} total · ${operations} operation${operations === 1 ? '' : 's'}`,
      details: names.map((name) => {
        const [n, time] = parts.get(name)!;
        return `${name}: ${n}, ${spaced(time)} total`;
      }),
      label: null,
      last: false,
    };
    carried += bar.ms;
    if (nextStart === null || Math.max(0, nextStart - end) >= close) {
      bar.label = laneTime(carried);
      carried = 0;
    }
    return bar;
  });
}

function roundView(state: State, round: Round, tuning: Tuning): RoundView {
  // The round's spans are back to back from its start, so the lanes' times
  // add up to how far it has got.
  const elapsed = sum(round.units.map((unit) => unit.ms));
  const scale = round.to_budget
    ? Math.max(round.budget_ms, elapsed)
    : Math.max(MIN_SCALE_MS, Math.floor((elapsed * 115) / 100));
  const stateLine = round.finished
    ? round.finished.backlogged
      ? 'Ended with work left: the next round starts at once.'
      : 'Ended. Sleeping until the poll interval is up or the node announces a block.'
    : round.woken_by === 'interval' ? 'Running, after the poll interval.'
      : round.woken_by === 'new_block' ? 'Running, woken by the node announcing a block.'
        : state.ribbon.length === 0 ? 'Running.'
          : 'Running, started at once: work was left.';
  const lanes: Lane[] = TIERS.map((tier, index) => {
    const share = tuning.shares[index] ?? 0;
    const laneMs = sum(round.units.filter((unit) => unit.tier === tier).map((unit) => unit.ms));
    const bars = segments(round, tier, scale);
    // The reserved share is drawn from where the tier's first segment
    // starts, so the outline goes round its segments rather than across one.
    const started = round.units.some((unit) => unit.tier === tier && unit.span.kind === 'unit' && unit.span.pass === 1)
      ? bars[0]?.start_ms ?? null
      : null;
    const ended = round.ended[index] ?? null;
    return {
      tier,
      name: tierName(tier),
      share: `${share} %`,
      ms: laneMs,
      time: laneTime(laneMs),
      bars,
      reserved: round.to_budget && started !== null ? [started, Math.floor((round.budget_ms * share) / 100)] : null,
      outcome: ended === null ? null : outcomeChip(ended),
    };
  });
  // The end marker goes on the segment that finished last (the latest to
  // start, of those ending last).
  let last: Bar | null = null;
  for (const bar of lanes.flatMap((lane) => lane.bars)) {
    if (!last || bar.start_ms + bar.span_ms > last.start_ms + last.span_ms
      || (bar.start_ms + bar.span_ms === last.start_ms + last.span_ms && bar.start_ms >= last.start_ms)) last = bar;
  }
  if (last) last.last = true;
  return {
    number: round.number,
    title: `Round ${thousands(round.number)}`,
    state: stateLine,
    scale_ms: scale,
    elapsed_ms: elapsed,
    elapsed: laneTime(elapsed),
    lanes,
  };
}

const WAIT_WORDS: Record<Wait, string> = {
  chain_height_unknown: 'the chain height is unknown',
  reorg_being_reconciled: 'a reorganisation is being reconciled',
  rewound_this_round: 'rewound this round',
  node_failed: 'the node failed',
  node_cannot_serve_tip: "the node can't serve its tip yet",
  mempool_unreadable: "the pool couldn't be read",
  reorg_candidates_retrying: 'payments to re-examine are waiting to retry',
  chain_diverged: "the node's chain differs from the recorded one",
};

/** A tier's ending as a chip. */
export function outcomeChip(outcome: TierOutcome): Chip {
  if (typeof outcome === 'object') return { tone: 'warn', text: `Waiting: ${WAIT_WORDS[outcome.blocked]}` };
  switch (outcome) {
    case 'idle': return { tone: 'ok', text: 'Idle' };
    case 'backlogged': return { tone: 'hi', text: 'Backlogged: out of time with work left' };
    case 'failed': return { tone: 'err', text: 'Failed: retried next round' };
  }
}

/** 4 to 28 pixels: 4 for a millisecond, 28 for ten seconds or more. */
export function ribbonHeight(ms: number): number {
  const scaled = Math.log10(Math.max(1, ms)) / 4;
  return Math.round(4 + 24 * Math.min(1, Math.max(0, scaled)));
}

function ribbonMark(entry: RibbonEntry): RibbonMark {
  if (entry.kind === 'sleep') {
    return {
      kind: 'sleep',
      woken: entry.woken_by === 'new_block',
      title: entry.woken_by === 'new_block'
        ? `Slept ${seconds(entry.ms)}, cut short by a new block`
        : `Slept ${seconds(entry.ms)}, the poll interval`,
    };
  }
  const total = sum(entry.tiers_ms);
  return {
    kind: 'round',
    number: entry.number,
    height: ribbonHeight(entry.ms),
    parts: TIERS.flatMap((tier, i): [Tier, number][] => (entry.tiers_ms[i] > 0 ? [[tier, fraction(entry.tiers_ms[i], total)]] : [])),
    title: `Round ${thousands(entry.number)}: ${seconds(entry.ms)}${entry.backlogged ? ', work was left' : ''}`,
  };
}

const queue = (queued: number, capacity: number) => `${thousands(queued)} of ${thousands(capacity)}`;

function side(state: State): Side {
  const { reorg, orders, pool, database } = state;
  const waiting = orders.pending + orders.due;
  const queued = sum(database.queued);
  const replaced = state.chain.replaced.length > 0;
  const nodePool = state.node_pool;
  const found = pool.found > 0 ? `, ${plural(pool.found, 'payment')} found` : '';
  return {
    reorg: {
      summary: reorg
        ? `Reorg from ${thousands(reorg.fork)}: ${reorg.step === 'found' ? 'found' : reorg.step === 'collecting' ? 'collecting payments' : 're-examining payments'}`
        : replaced ? "The node's chain differs: waiting to reconcile"
          : state.chain_agrees === true && state.chain.high_water !== null ? `Agrees with the node at ${thousands(state.chain.high_water)}`
            : 'No reorganisation',
      alert: reorg !== null || replaced,
      rows: [
        ['Fork at', reorg ? thousands(reorg.fork) : DASH],
        ['Payments left to re-examine', reorg && reorg.candidates !== null ? thousands(reorg.candidates) : DASH],
        ['Re-examined, changed, voided', reorg ? `${thousands(reorg.examined)}, ${thousands(reorg.changed)}, ${thousands(reorg.voided)}` : DASH],
      ],
    },
    mempool: {
      summary: nodePool
        ? pool.watched ? `${thousands(nodePool.txs)} in the node's pool${found}` : `${thousands(nodePool.txs)} in the node's pool, not scanned`
        : pool.watched ? `${thousands(pool.remembered)} scanned in the pool${found}` : 'Not scanned: no order waits to be paid',
      alert: false,
      rows: [
        ["In the node's pool", nodePool ? thousands(nodePool.txs) : DASH],
        ["Of the next block's full-reward size", nodePool && nodePool.bytes !== null && nodePool.penalty_free !== null
          ? `${((nodePool.bytes / Math.max(1, nodePool.penalty_free)) * 100).toFixed(0)} %` : DASH],
        ['Scanned by the engine', pool.watched ? 'yes' : 'no: no order waits to be paid'],
        ['Transactions scanned and remembered', thousands(pool.remembered)],
        ['Fast passes with something new, last minute', thousands(count(pool.fast_passes))],
        ['Payments found', thousands(pool.found)],
      ],
    },
    orders: {
      summary: reorg && waiting > 0 ? 'paid held while the reorg is open'
        : waiting > 0 ? `${thousands(waiting)} to recompute`
          : 'nothing to recompute',
      alert: false,
      rows: [
        ['A payment changed', thousands(orders.pending)],
        ['Due by time or height', thousands(orders.due)],
        ['Unconfirmed payments checked last', thousands(orders.looked)],
      ],
    },
    upkeep: {
      summary: state.upkeep.round === null ? 'every round, in the background' : `ran in round ${thousands(state.upkeep.round)}`,
      alert: false,
      rows: [['Old block hashes pruned last time', thousands(state.upkeep.pruned)]],
    },
    database: {
      summary: `${thousands(queued)} queued, ${thousands(database.completed)} jobs done`,
      alert: false,
      rows: [
        ['Scanner queue', queue(database.queued[0], database.capacity)],
        ['Admin queue', queue(database.queued[1], database.capacity)],
        ['Longest wait for a turn', micros(database.max_queue_wait_us)],
        ['Longest job', micros(database.max_run_us)],
      ],
    },
    restart: {
      summary: `${plural(count(state.saves), 'save')} a minute; ${plural(state.chain.cached.length, 'block')} only in memory`,
      alert: false,
      rows: [
        ['Store cursors saved', thousands(state.saved.cursors)],
        ['Block checkpoints saved', thousands(state.saved.checkpoints)],
        ['Reorg job steps saved', thousands(state.saved.reorg)],
        ['Payments recorded', thousands(state.saved.payments)],
        ['Recomputes saved', thousands(state.saved.recomputes)],
        ['In memory: block cache', megabytes(state.chain.cache_bytes)],
        ['In memory: pool transactions', thousands(pool.remembered)],
      ],
    },
    reorg_step: reorg ? (reorg.step === 'found' ? 'found' : reorg.step === 'collecting' ? 'collect' : 'process') : null,
    pool_txs: pool.txs.map((tx): [string, boolean] => [tx.txid, tx.matched]),
    transitions: orders.last.map((t): [OrderStatus, OrderStatus] => [t.from, t.to]),
    queues: database.queued,
    queue_capacity: database.capacity,
  };
}
