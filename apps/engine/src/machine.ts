// What the engine page's "Machine and links" strip and Scanning panel show,
// from each `machine` event's figures (wire.ts): the machine's CPU and
// memory over the last hour, the link to the node in use, the blocks' size
// and the scan (docs/engine_scaling.md section 6).

import { DASH, bytes, duration, durationRough, networkName, rate, thousands } from './format.ts';
import type {
  LinkPoint, LinkSnapshot, Machine, NetworkScaling, NodeInUse, ResourceReport, ResourceSample, SlowBlock, Trend,
} from './wire.ts';

/** One small multiple of the strip: a figure, a line under it and a small
 *  chart. */
export interface Tile {
  /** `cpu`, `memory`, `transfer`, `round-trip`, `first-byte`, `block-size`
   *  (with `engine-` or `monokulo-` before the first two when the processes
   *  are on different machines). */
  key: string;
  label: string;
  value: string;
  note: string;
  /** The tile in a sentence, for its tooltip and screen readers. */
  title: string;
  chart: TileChart;
}

export type TileChart =
  | { kind: 'none' }
  /** Bands stacked over an hour of 10-second slots, the first at the bottom
   *  (`viewBox="0 0 360 100"`), and limits dashed across. */
  | { kind: 'stack'; layers: { class: string; d: string }[]; limits: { class: string; y: string; label: string }[] }
  /** An hour at a point a minute (`viewBox="0 0 59 16"`): a line per run of
   *  measured minutes, a dot for a run of one. */
  | { kind: 'spark'; runs: string[] };

export interface ScanningView {
  /** "at the tip · pace: link speed", "14 behind · pace: memory budget". */
  preview: string;
  slow: string | null;
  rows: [string, string][];
}

export interface MachineView {
  /** CPU, Memory, Transfer, Round trip, First byte, Block size. */
  tiles: Tile[];
  /** The engine and monokulo are stacked in a chart: the strip's head says
   *  which colour is which. */
  stacked: boolean;
  scanning: ScanningView | null;
  /** When the engine wrote the `/status` this is from (its clock). */
  at_unix: number;
}

/** Each process samples itself into slots this long (`shared::resources`). */
export const SLOT_SECS = 10;
/** An hour of slots: what the charts show. */
export const SLOTS = 360;

/** The start of the chart's first slot, so its last one holds `now`. */
const firstSlot = (now: number) => Math.floor(now / SLOT_SECS) * SLOT_SECS - (SLOTS - 1) * SLOT_SECS;

/** One process's samples placed on the hour's slots ending at `now`: null
 *  for a slot it has no sample for, which the chart leaves as a gap. */
export function onSlots(samples: ResourceSample[], now: number, value: (s: ResourceSample) => number): (number | null)[] {
  const first = firstSlot(now);
  const slots: (number | null)[] = new Array(SLOTS).fill(null);
  for (const sample of samples) {
    const at = Math.floor((sample.unix - first) / SLOT_SECS);
    if (at >= 0 && at < SLOTS) slots[at] = value(sample);
  }
  return slots;
}

/** "3 minutes ago", or "just now" within the last slot. */
const ago = (secs: number) => (secs < SLOT_SECS ? 'just now' : `${durationRough(secs)} ago`);

const percent = (value: number) => `${value.toFixed(0)} %`;
const memory = (value: number) => bytes(Math.trunc(Math.max(0, value)));
const sum = (values: number[]) => values.reduce((total, n) => total + n, 0);

interface Layer {
  /** Its colour role's class, `chart-engine` or `chart-monokulo`. */
  class: string;
  name: string;
  slots: (number | null)[];
}

/** A chart of `layers` stacked in order (the first at the bottom), scaled so
 *  `max` is the top. A slot is drawn only where every layer has a sample. */
interface Chart {
  what: string;
  layers: Layer[];
  max: number;
  format: (value: number) => string;
  limits: { value: number; class: string; label: string }[];
  now: number;
}

/** Each drawable slot's layer values, null for a gap. */
function stacks(chart: Chart): (number[] | null)[] {
  return Array.from({ length: SLOTS }, (_, slot) => {
    const values = chart.layers.map((layer) => layer.slots[slot] ?? null);
    return values.every((value) => value !== null) ? (values as number[]) : null;
  });
}

const y = (chart: Chart, value: number) =>
  chart.max <= 0 ? 100 : Math.min(100, Math.max(0, 100 - (value / chart.max) * 100));

/** The SVG path of layer `k`: a step outline per run of drawn slots, from
 *  the layers below it to its own top. */
function path(chart: Chart, drawn: (number[] | null)[], k: number): string {
  let d = '';
  let slot = 0;
  while (slot < drawn.length) {
    if (!drawn[slot]) { slot++; continue; }
    const start = slot;
    while (slot < drawn.length && drawn[slot]) slot++;
    const below = (at: number) => sum(drawn[at]!.slice(0, k));
    const top = (at: number) => below(at) + drawn[at]![k];
    const points: string[] = [];
    for (let at = start; at < slot; at++) {
      const v = y(chart, top(at)).toFixed(1);
      points.push(`${at},${v}`, `${at + 1},${v}`);
    }
    for (let at = slot - 1; at >= start; at--) {
      const v = y(chart, below(at)).toFixed(1);
      points.push(`${at + 1},${v}`, `${at},${v}`);
    }
    // Two slots at one height meet at a point both would add.
    const unique = points.filter((point, i) => i === 0 || point !== points[i - 1]);
    d += `M${unique.join('L')}Z`;
  }
  return d;
}

/** "25 % (engine 23 %, monokulo 2 %)": a total and, when stacked, its parts. */
function split(chart: Chart, values: number[]): string {
  const total = sum(values);
  if (chart.layers.length < 2) return chart.format(total);
  return `${chart.format(total)} (${chart.layers.map((layer, i) => `${layer.name} ${chart.format(values[i])}`).join(', ')})`;
}

/** The chart as a tile: the total now, what it is of and the hour's peak,
 *  and the stacked bands with any limit dashed across them. */
function chartTile(chart: Chart, key: string, capacity: string): Tile {
  const drawn = stacks(chart);
  let value: string, note: string, title: string;
  const current = [...drawn].reverse().find((values) => values !== null);
  if (current) {
    let peakAt = 0, peak = -Infinity;
    drawn.forEach((values, at) => {
      if (values && sum(values) > peak) [peakAt, peak] = [at, sum(values)];
    });
    const peakAgo = chart.now - (firstSlot(chart.now) + peakAt * SLOT_SECS);
    const peakText = `peak ${chart.format(peak)}`;
    value = chart.format(sum(current));
    note = capacity ? `${capacity} · ${peakText}` : peakText;
    title = `${chart.what} over the last hour, now ${split(chart, current)}${capacity ? ` ${capacity}` : ''}; peak ${chart.format(peak)} ${ago(peakAgo)}`;
  } else {
    [value, note, title] = [DASH, 'no samples yet', `${chart.what}: no samples yet`];
  }
  for (const limit of chart.limits) title += `. ${limit.label} (dashed)`;
  return {
    key,
    label: chart.what,
    value,
    note,
    title,
    chart: {
      kind: 'stack',
      layers: chart.layers.map((layer, k) => ({ class: layer.class, d: path(chart, drawn, k) })),
      limits: chart.limits.map((limit) => ({ class: limit.class, y: y(chart, limit.value).toFixed(1), label: limit.label })),
    },
  };
}

type Process = [name: string, cls: string, report: ResourceReport];
const ENGINE = ['engine', 'chart-engine'] as const;
const MONOKULO = ['monokulo', 'chart-monokulo'] as const;

/** The CPU and memory tiles for `processes` stacked (first at the bottom),
 *  all on one machine, their labels and keys after `who` ("Engine" when the
 *  processes are apart). With `sharedMemory`, the processes are one process:
 *  memory is the last one's, under that name. */
function machineTiles(processes: Process[], now: number, sharedMemory: string | null, who: string | null): Tile[] {
  const machine = processes[0]?.[2];
  if (!machine) return [];
  const [cpuLabel, memoryLabel, cpuKey, memoryKey] = who
    ? [`${who} CPU`, `${who} memory`, `${who.toLowerCase()}-cpu`, `${who.toLowerCase()}-memory`]
    : ['CPU', 'Memory', 'cpu', 'memory'];
  const cores = machine.cpu_count === 1 ? 'of 1 core' : `of ${machine.cpu_count} cores`;
  const cpu: Chart = {
    what: cpuLabel,
    layers: processes.map(([name, cls, report]) => ({ class: cls, name, slots: onSlots(report.samples, now, (s) => s.cpu_percent) })),
    max: 100,
    format: percent,
    limits: [],
    now,
  };
  const totalMemory = machine.machine_memory_bytes ?? 0;
  const memoryOf: Process[] = sharedMemory
    ? processes.slice(-1).map(([, cls, report]): Process => [sharedMemory, cls, report])
    : processes;
  const ram: Chart = {
    what: memoryLabel,
    layers: memoryOf.map(([name, cls, report]) => ({ class: cls, name, slots: onSlots(report.samples, now, (s) => s.memory_bytes) })),
    max: totalMemory,
    format: memory,
    limits: processes.flatMap(([name, cls, report]) => {
      const limit = report.cgroup_memory_bytes;
      return limit !== null && limit < totalMemory
        ? [{ value: limit, class: cls, label: `The ${name}'s container limit: ${bytes(limit)}` }]
        : [];
    }),
    now,
  };
  const ofRam = machine.machine_memory_bytes !== null ? `of ${bytes(machine.machine_memory_bytes)}` : '';
  return [chartTile(cpu, cpuKey, cores), chartTile(ram, memoryKey, ofRam)];
}

/** The strip's CPU and memory tiles: monokulo and the engine together, each
 *  a band of one stacked chart, when they share a machine; a pair of tiles
 *  each when they don't. */
export function resourceTiles(machine: Machine): Tile[] {
  const { engine, monokulo, one_process } = machine.resources;
  const now = machine.now_unix;
  const mono: Process = [MONOKULO[0], MONOKULO[1], monokulo];
  if (engine && one_process) return machineTiles([[ENGINE[0], ENGINE[1], engine], mono], now, 'monokulo and the engine', null);
  if (engine && engine.host_id === monokulo.host_id) return machineTiles([[ENGINE[0], ENGINE[1], engine], mono], now, null, null);
  if (engine) {
    return [
      ...machineTiles([[ENGINE[0], ENGINE[1], engine]], now, null, 'Engine'),
      ...machineTiles([mono], now, null, 'Monokulo'),
    ];
  }
  // The engine didn't report its own: monokulo's alone.
  return machineTiles([mono], now, null, 'Monokulo');
}

/** An hour of one of a node's figures, a point a minute, as runs of measured
 *  minutes ("x,y x,y …"); a run of one minute is one point. */
function sparkRuns(history: LinkPoint[], now: number, value: (p: LinkPoint) => number): string[] {
  const MINUTES = 60;
  const thisMinute = Math.floor(now / 60) * 60;
  const points: (number | null)[] = new Array(MINUTES).fill(null);
  for (const point of history) {
    const back = Math.floor((thisMinute - point.minute_unix) / 60);
    if (back >= 0 && back < MINUTES) points[MINUTES - 1 - back] = value(point);
  }
  const max = Math.max(1, ...points.filter((p): p is number => p !== null));
  const runs: string[] = [];
  let run: string[] = [];
  points.forEach((point, x) => {
    if (point !== null) run.push(`${x},${(15 - (point / max) * 14).toFixed(1)}`);
    else if (run.length) { runs.push(run.join(' ')); run = []; }
  });
  if (run.length) runs.push(run.join(' '));
  return runs;
}

const counted = (n: number, one: string, many: string) => (n === 1 ? `1 ${one}` : `${n} ${many}`);

type Figure = [key: string, label: string, value: (p: LinkPoint) => number, format: (n: number) => string];
const LINK_FIGURES: Figure[] = [
  ['transfer', 'Transfer', (p) => p.rate_bytes_per_sec, rate],
  ['round-trip', 'Round trip', (p) => p.rtt_ms, (ms) => `${ms} ms`],
  ['first-byte', 'First byte', (p) => p.ttfb_per_block_ms, (ms) => `${ms} ms`],
];

/** The Transfer, Round trip and First byte tiles: what the engine measured of
 *  the link to the node it is using now, each with its last hour, the tile
 *  naming the node. With no node in use there is nothing to measure. */
export function linkTiles(node: NodeInUse | null, now: number): Tile[] {
  return LINK_FIGURES.map(([key, label, value, format]): Tile => {
    if (!node) {
      return {
        key, label, value: DASH, note: 'no node in use',
        title: `${label}: the engine isn't using a node on this network right now.`, chart: { kind: 'none' },
      };
    }
    const name = node.fallback ? `${node.label} · fallback` : node.label;
    const link: LinkSnapshot | null = node.link;
    if (link && link.measured) {
      const trouble = link.timeouts_last_hour === 0 && link.failures_last_hour === 0
        ? 'no timeouts or failures in the last hour'
        : `${counted(link.timeouts_last_hour, 'timeout', 'timeouts')} and ${counted(link.failures_last_hour, 'failure', 'failures')} in the last hour`;
      const measured = link.last_measured_unix !== null ? `measured ${ago(now - link.last_measured_unix)}; ` : '';
      const what = key === 'first-byte' ? 'First byte of a block' : label;
      const figure = format(value({ minute_unix: now, ...link }));
      return {
        key, label, value: figure, note: name,
        title: `${what} from ${name}: ${figure} now, the last hour drawn; ${measured}${trouble}.`,
        chart: { kind: 'spark', runs: sparkRuns(link.history, now, value) },
      };
    }
    return {
      key, label, value: DASH, note: name,
      title: `${label} from ${name}: not measured yet. The engine assumes ${rate(link?.rate_bytes_per_sec ?? 0)} until its first block request.`,
      chart: { kind: 'none' },
    };
  });
}

const TRENDS: Record<Trend, string> = { rising: 'rising ↗', falling: 'falling ↘', steady: 'steady' };

/** The Block size tile: the average of recent blocks and which way it's going. */
export function blockSizeTile(scaling: NetworkScaling | null): Tile {
  if (!scaling) {
    return {
      key: 'block-size', label: 'Block size', value: DASH, note: 'not reported',
      title: "The engine hasn't reported its block sizes.", chart: { kind: 'none' },
    };
  }
  const trend = TRENDS[scaling.scan.block_size_trend];
  const average = bytes(scaling.scan.avg_block_bytes);
  return {
    key: 'block-size', label: 'Block size', value: average, note: `average, ${trend}`,
    title: `Recent blocks average ${average}, ${trend}.`, chart: { kind: 'none' },
  };
}

/** One sentence about a slow block: what is slow, at what rate, how much
 *  longer it needs and what would help (docs/engine_scaling.md section 5).
 *  The Status page writes the same in Rust (`views::scaling`). */
export function slowBlockMessage(network: string, slow: SlowBlock): string {
  const size = slow.wire_bytes !== null ? ` (${bytes(slow.wire_bytes)})` : '';
  let message = `${networkName(network)}: block ${thousands(slow.height)}${size} has taken ${duration(slow.elapsed_secs)} so far`;
  if (slow.rate_bytes_per_sec !== null) {
    message += slow.node !== null ? `, at ${rate(slow.rate_bytes_per_sec)} from ${slow.node}` : `, at ${rate(slow.rate_bytes_per_sec)}`;
  }
  message += '.';
  if (slow.remaining_secs !== null && slow.remaining_secs > 0) {
    message += ` At this rate it needs about ${durationRough(slow.remaining_secs)} more.`;
  }
  return `${message} A faster node or a larger scan memory budget would help.`;
}

const blocks = (n: number) => (n === 1 ? '1 block' : `${thousands(n)} blocks`);

const CHUNK_LIMITS = {
  memory: 'the memory budget',
  link: "the link's speed",
  maximum: 'the most one request asks for',
  remaining: 'the blocks left',
  cpu: 'the time scanning takes',
} as const;

/** One network's Scanning panel: a short line for its summary, a slow block
 *  if there is one, and its figures. */
export function scanning(network: string, scaling: NetworkScaling, node: NodeInUse | null): ScanningView {
  const scan = scaling.scan;
  const progressShort = scaling.blocks_behind === 0 ? 'at the tip' : `${thousands(scaling.blocks_behind)} behind`;
  const paceShort = scaling.slow ? 'a slow block' : {
    caught_up: null, memory: 'pace: memory budget', link: 'pace: link speed', cpu: 'pace: CPU', round: 'pace: round time',
  }[scaling.pace];
  let progress: string;
  if (scaling.blocks_behind === 0) progress = "At the node's tip.";
  else {
    progress = `${blocks(scaling.blocks_behind)} behind · ${scan.blocks_per_minute.toFixed(1)} blocks a minute`;
    if (scaling.catch_up_secs !== null) progress += ` · caught up in about ${durationRough(scaling.catch_up_secs)}`;
  }
  // The rate the node in use was measured at, if it was.
  const speed = node?.link?.measured ? node.link.rate_bytes_per_sec : null;
  const pace = {
    caught_up: "Nothing: the scan is at the node's tip.",
    memory: 'The scan memory budget, which caps each request. A larger budget (Server tab) would speed it up.',
    link: node
      ? speed !== null
        ? `Link speed: ${node.label} at ${rate(speed)}. A faster node would speed it up.`
        : `Link speed: ${node.label}. A faster node would speed it up.`
      : 'Link speed. A faster node would speed it up.',
    cpu: 'Scanning for stores (CPU), which takes longer than fetching. A faster machine would speed it up.',
    round: "The round's time for blocks, shared with payments and checks.",
  }[scaling.pace];
  let request = scan.last_chunk
    ? `${blocks(scan.last_chunk.blocks)} a request, set by ${CHUNK_LIMITS[scan.last_chunk.limited_by]}`
    : 'None yet';
  if (scan.in_progress && scan.in_progress_secs !== null) {
    request += ` · block ${thousands(scan.in_progress.height)} in progress for ${duration(scan.in_progress_secs)}`;
    const pages = scan.in_progress.pages;
    if (pages) request += `, in pages: ${thousands(pages.done_txs)} of ${thousands(pages.total_txs)} transactions scanned, ${thousands(pages.page_txs)} a page`;
  }
  let blockSize = `Average ${bytes(scan.avg_block_bytes)} (${TRENDS[scan.block_size_trend]})`;
  if (scan.largest_recent) {
    const largest = scan.largest_recent;
    blockSize += ` · largest recent ${bytes(largest.wire_bytes)} (block ${thousands(largest.height)}), took ${duration(Math.round(largest.secs))}`;
  }
  let memoryLine = `Budget ${thousands(scaling.budget_mb)} MB`;
  if (scan.peak_cache_bytes !== null) memoryLine += ` · block cache peak ${bytes(scan.peak_cache_bytes)} in the last hour`;
  if (scaling.max_budget_mb !== null) memoryLine += ` · this machine allows up to ${thousands(scaling.max_budget_mb)} MB`;
  // Blocks fetched ahead and let go of before their scan were fetched for
  // nothing; a larger budget would have kept them.
  const discarded = scan.discarded_cache_bytes_recent ?? 0;
  if (discarded > 0) {
    memoryLine += ` · ${bytes(discarded)} fetched ahead was let go of unscanned in the last ten minutes, to be fetched again (a larger budget keeps more)`;
  }
  const round = scaling.round_deadline_secs > scaling.round_base_secs
    ? `Deadline ${scaling.round_deadline_secs} s, raised from ${scaling.round_base_secs} s so one large block's work fits`
    : `Deadline ${scaling.round_deadline_secs} s (the base)`;
  const headersFirst = scan.headers_first
    ? `On for about ${durationRough(scan.headers_first.remaining_secs)} more: ${scan.headers_first.reason === 'failed_request'
      ? 'a block request ran out of time or came back too large'
      : 'a recent block came near the size that is scanned in pages'}, so each block's size is checked before it is fetched`
    : 'Off: blocks are fetched whole without asking their size first';
  return {
    preview: paceShort ? `${progressShort} · ${paceShort}` : progressShort,
    slow: scaling.slow ? slowBlockMessage(network, scaling.slow) : null,
    rows: [
      ['Progress', progress],
      ['Pace set by', pace],
      ['Requests', request],
      ['Block size', blockSize],
      ['Memory', memoryLine],
      ['Round', round],
      ['Headers first', headersFirst],
      ['Last 10 minutes', `Fetching ${duration(Math.round(scan.fetch_secs_recent))}, scanning ${duration(Math.round(scan.scan_secs_recent))}`],
    ],
  };
}

/** Everything the strip and the Scanning panel show, from one `machine` event. */
export function machineView(machine: Machine): MachineView {
  const tiles = [...resourceTiles(machine), ...linkTiles(machine.node, machine.now_unix), blockSizeTile(machine.scaling)];
  return {
    tiles,
    stacked: tiles.some((tile) => tile.chart.kind === 'stack' && tile.chart.layers.length > 1),
    scanning: machine.scaling ? scanning(machine.network, machine.scaling, machine.node) : null,
    at_unix: machine.at_unix,
  };
}
