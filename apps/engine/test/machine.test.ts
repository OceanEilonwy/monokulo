// The "Machine and links" strip and the Scanning panel, from the figures of a
// `machine` event.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { bytes, duration, durationRough, rate, thousands } from '../src/format.ts';
import type { Tile } from '../src/machine.ts';
import { SLOT_SECS, SLOTS, blockSizeTile, linkTiles, machineView, onSlots, resourceTiles, scanning } from '../src/machine.ts';
import type { LinkSnapshot, Machine, NetworkScaling, ResourceReport, ResourceSample } from '../src/wire.ts';

const NOW = 1_800_000_000;

const report = (host: string, samples: ResourceSample[], more: Partial<ResourceReport> = {}): ResourceReport => ({
  host_id: host, cpu_count: 4, machine_memory_bytes: 8_000_000_000, cgroup_memory_bytes: null, samples, ...more,
});
/** A sample `back` slots before the newest, at `cpu` % and `mb` MB. */
const sample = (back: number, cpu: number, mb: number): ResourceSample => ({
  unix: NOW - (NOW % SLOT_SECS) - back * SLOT_SECS, cpu_percent: cpu, memory_bytes: mb * 1_000_000,
});
const machine = (resources: Machine['resources'], more: Partial<Machine> = {}): Machine => ({
  network: 'mainnet', at_unix: NOW, now_unix: NOW, resources, node: null, scaling: null, ...more,
});
const stack = (tile: Tile) => {
  assert.equal(tile.chart.kind, 'stack', `not a stacked chart: ${JSON.stringify(tile.chart)}`);
  return tile.chart as Extract<Tile['chart'], { kind: 'stack' }>;
};

test('samples land on their slot, and missing ones are gaps', () => {
  const slots = onSlots([sample(0, 5, 1), sample(2, 7, 1), sample(400, 9, 1)], NOW, (s) => s.cpu_percent);
  assert.equal(slots.length, SLOTS);
  assert.equal(slots[SLOTS - 1], 5);
  assert.equal(slots[SLOTS - 2], null, 'a slot nobody sampled is a gap, not zero');
  assert.equal(slots[SLOTS - 3], 7);
  assert.equal(slots.filter((slot) => slot !== null).length, 2, 'older than an hour is dropped');
});

test('one machine stacks the engine under monokulo, with the total and its split', () => {
  const tiles = resourceTiles(machine({
    engine: report('boot-1', [sample(7, 60, 500), sample(1, 23, 412), sample(0, 23, 412)], { cgroup_memory_bytes: 2_000_000_000 }),
    monokulo: report('boot-1', [sample(7, 3, 90), sample(1, 2, 96), sample(0, 2, 96)]),
    one_process: false,
  }));
  assert.deepEqual(tiles.map((t) => t.key), ['cpu', 'memory']);
  const [cpu, ram] = tiles;
  assert.deepEqual([cpu.label, cpu.value], ['CPU', '25 %']);
  assert.equal(cpu.note, 'of 4 cores · peak 63 %');
  assert.equal(cpu.title, 'CPU over the last hour, now 25 % (engine 23 %, monokulo 2 %) of 4 cores; peak 63 % 70 seconds ago');
  assert.equal(ram.value, '508 MB');
  assert.equal(ram.note, 'of 8.0 GB · peak 590 MB');
  assert.ok(ram.title.startsWith('Memory over the last hour, now 508 MB (engine 412 MB, monokulo 96.0 MB) of 8.0 GB'), ram.title);
  // The engine's container limit is a dashed line on the memory chart.
  assert.ok(ram.title.endsWith("The engine's container limit: 2.0 GB (dashed)"), ram.title);
  const memory = stack(ram);
  assert.deepEqual(memory.limits.map((l) => [l.class, l.y]), [['chart-engine', '75.0']]);
  assert.deepEqual(memory.layers.map((l) => l.class), ['chart-engine', 'chart-monokulo'], 'the engine is drawn first, at the bottom');
  // Two runs (seven slots back, then the last two), so each band's path has
  // two parts.
  const processor = stack(cpu);
  assert.deepEqual(processor.limits, []);
  assert.equal(processor.layers[0].d.split('M').length - 1, 2, processor.layers[0].d);
});

test("the engine inside monokulo: CPU split by the engine's threads, memory once", () => {
  // As monokulo sends it: one process's report split by thread, the
  // memory in both.
  const tiles = resourceTiles(machine({
    engine: report('boot-1', [sample(1, 20, 300), sample(0, 20, 300)]),
    monokulo: report('boot-1', [sample(1, 5, 300), sample(0, 5, 300)]),
    one_process: true,
  }));
  assert.ok(tiles[0].title.includes('now 25 % (engine 20 %, monokulo 5 %) of 4 cores'), tiles[0].title);
  assert.equal(tiles[1].value, '300 MB', 'once, not 600 MB');
  assert.equal(stack(tiles[1]).layers.length, 1, 'one band for memory');
});

test('different machines are shown apart, and a silent engine says so', () => {
  const apart = resourceTiles(machine({
    engine: report('boot-1', [sample(0, 23, 412)]),
    monokulo: report('boot-2', [sample(0, 2, 96)]),
    one_process: false,
  }));
  assert.deepEqual(apart.map((t) => t.label), ['Engine CPU', 'Engine memory', 'Monokulo CPU', 'Monokulo memory']);
  assert.equal(apart[0].value, '23 %');
  assert.equal(apart[0].key, 'engine-cpu');
  assert.equal(stack(apart[0]).layers.length, 1, 'nothing is stacked');
  const silent = resourceTiles(machine({ engine: null, monokulo: report('boot-2', []), one_process: false }));
  assert.equal(silent[0].label, 'Monokulo CPU');
  assert.equal(silent[0].note, 'no samples yet');
});

const link = (measured: boolean): LinkSnapshot => ({
  measured, rtt_ms: 120, ttfb_per_block_ms: 45, rate_bytes_per_sec: 387_500, bytes_per_block: 50_000,
  last_measured_unix: measured ? NOW - 12 : null, timeouts_last_hour: 2, failures_last_hour: 1,
  history: [
    { minute_unix: Math.floor(NOW / 60) * 60, rate_bytes_per_sec: 387_500, rtt_ms: 120, ttfb_per_block_ms: 45 },
    { minute_unix: Math.floor(NOW / 60) * 60 - 60, rate_bytes_per_sec: 300_000, rtt_ms: 110, ttfb_per_block_ms: 40 },
    { minute_unix: Math.floor(NOW / 60) * 60 - 600, rate_bytes_per_sec: 100_000, rtt_ms: 90, ttfb_per_block_ms: 30 },
  ],
});

test('the link tiles follow the node in use, naming it, a fallback said so; each figure has its hour', () => {
  const tiles = linkTiles({ label: 'node.example.com:18081', fallback: false, link: link(true) }, NOW);
  assert.deepEqual(tiles.map((t) => [t.label, t.value, t.note]), [
    ['Transfer', '3.1 Mbit/s', 'node.example.com:18081'],
    ['Round trip', '120 ms', 'node.example.com:18081'],
    ['First byte', '45 ms', 'node.example.com:18081'],
  ]);
  assert.equal(tiles[0].title, 'Transfer from node.example.com:18081: 3.1 Mbit/s now, the last hour drawn; measured 12 seconds ago; 2 timeouts and 1 failure in the last hour.');
  // The two latest minutes are a line; ten minutes back, alone, a dot.
  const spark = tiles[1].chart;
  assert.equal(spark.kind, 'spark');
  if (spark.kind === 'spark') {
    assert.equal(spark.runs.length, 2);
    assert.ok(!spark.runs[0].includes(' ') && spark.runs[1].includes(' '), JSON.stringify(spark.runs));
  }
  const fallback = linkTiles({ label: 'backup.example.org:18089', fallback: true, link: link(true) }, NOW);
  assert.equal(fallback[0].note, 'backup.example.org:18089 · fallback');
  const unmeasured = linkTiles({ label: 'node.example.com:18081', fallback: false, link: link(false) }, NOW);
  assert.equal(unmeasured[0].value, '–');
  assert.ok(unmeasured[0].title.includes('not measured yet. The engine assumes 3.1 Mbit/s'), unmeasured[0].title);
  assert.deepEqual(unmeasured[0].chart, { kind: 'none' });
  const none = linkTiles(null, NOW);
  assert.equal(none.length, 3);
  assert.ok(none.every((t) => t.note === 'no node in use'));
});

const scaling = (pace: NetworkScaling['pace']): NetworkScaling => ({
  scan: {
    avg_block_bytes: 1_800_000,
    block_size_trend: 'rising',
    last_chunk: { blocks: 1, limited_by: 'link' },
    blocks_per_minute: 3.2,
    fetch_secs_recent: 400,
    scan_secs_recent: 12,
    largest_recent: { height: 3_411_990, wire_bytes: 412_000_000, secs: 1_080, finished_unix: NOW - 100 },
    in_progress: { height: 3_412_001, started_unix: NOW - 130, wire_bytes: null, pages: null },
    in_progress_secs: 130,
    peak_cache_bytes: 301_000_000,
    discarded_cache_bytes_recent: 0,
    round_budget_secs: null,
    headers_first: null,
  },
  blocks_behind: 14,
  catch_up_secs: 240,
  pace,
  budget_mb: 256,
  max_budget_mb: 1_536,
  round_deadline_secs: 10,
  round_base_secs: 10,
  slow: null,
});
const node = { label: 'node.example:18089', fallback: false, link: { ...link(true), rate_bytes_per_sec: 387_500 } };
const row = (view: ReturnType<typeof scanning>, label: string) => {
  const found = view.rows.find(([l]) => l === label);
  assert.ok(found, `no ${label} in ${JSON.stringify(view)}`);
  return found[1];
};

test('the Scanning panel says what sets the pace, and why', () => {
  let view = scanning('mainnet', scaling('link'), node);
  assert.equal(view.preview, '14 behind · pace: link speed');
  assert.equal(row(view, 'Progress'), '14 blocks behind · 3.2 blocks a minute · caught up in about 4 minutes');
  assert.equal(row(view, 'Pace set by'), 'Link speed: node.example:18089 at 3.1 Mbit/s. A faster node would speed it up.');
  assert.equal(row(view, 'Requests'), "1 block a request, set by the link's speed · block 3,412,001 in progress for 2 m 10 s");
  assert.equal(row(view, 'Block size'), 'Average 1.8 MB (rising ↗) · largest recent 412 MB (block 3,411,990), took 18 m 0 s');
  assert.equal(row(view, 'Memory'), 'Budget 256 MB · block cache peak 301 MB in the last hour · this machine allows up to 1,536 MB');
  const discarding = scaling('memory');
  discarding.scan.discarded_cache_bytes_recent = 9_400_000;
  view = scanning('mainnet', discarding, node);
  assert.equal(view.preview, '14 behind · pace: memory budget');
  assert.ok(row(view, 'Memory').endsWith('this machine allows up to 1,536 MB · 9.4 MB fetched ahead was let go of unscanned in the last ten minutes, to be fetched again (a larger budget keeps more)'));
  assert.equal(row(view, 'Round'), 'Deadline 10 s (the base)');
  assert.equal(row(view, 'Headers first'), 'Off: blocks are fetched whole without asking their size first');
  assert.equal(row(view, 'Last 10 minutes'), 'Fetching 6 m 40 s, scanning 12 s');
  assert.ok(row(view, 'Pace set by').startsWith('The scan memory budget, which caps each request.'));

  const caughtUp = scaling('caught_up');
  caughtUp.blocks_behind = 0;
  caughtUp.round_deadline_secs = 45;
  view = scanning('mainnet', caughtUp, null);
  assert.equal(view.preview, 'at the tip');
  assert.equal(row(view, 'Progress'), "At the node's tip.");
  assert.ok(row(view, 'Round').startsWith('Deadline 45 s, raised from 10 s'));
  const tipByLink = scaling('link');
  tipByLink.blocks_behind = 0;
  assert.equal(scanning('mainnet', tipByLink, null).preview, 'at the tip · pace: link speed');

  // A large block scanned a page at a time says how far it has got, and why
  // headers come first.
  const paged = scaling('link');
  paged.scan.headers_first = { remaining_secs: 3_000, reason: 'failed_request' };
  paged.scan.in_progress!.pages = { done_txs: 41_000, total_txs: 97_000, page_txs: 100 };
  view = scanning('mainnet', paged, null);
  assert.ok(row(view, 'Requests').includes('in pages: 41,000 of 97,000 transactions scanned, 100 a page'));
  assert.ok(row(view, 'Headers first').startsWith('On for about 50 minutes more: a block request ran out of time or came back too large'));

  const slow = scaling('link');
  slow.slow = { height: 3_412_001, wire_bytes: 412_000_000, elapsed_secs: 130, node: 'node.example:18089', rate_bytes_per_sec: 387_500, remaining_secs: 1_080 };
  view = scanning('mainnet', slow, node);
  assert.equal(view.preview, '14 behind · a slow block');
  assert.ok(view.slow?.startsWith('Mainnet: block 3,412,001 (412 MB) has taken 2 m 10 s so far, at 3.1 Mbit/s from node.example:18089.'), view.slow ?? '');
  assert.ok(view.slow?.endsWith('At this rate it needs about 18 minutes more. A faster node or a larger scan memory budget would help.'), view.slow ?? '');
});

test('the Block size tile gives the average and its trend', () => {
  const tile = blockSizeTile(scaling('link'));
  assert.deepEqual([tile.value, tile.note], ['1.8 MB', 'average, rising ↗']);
  assert.equal(blockSizeTile(null).note, 'not reported');
});

test("a machine event's view: the tiles in order, whether they stack, the Scanning panel", () => {
  const view = machineView(machine({
    engine: report('boot-1', [sample(0, 23, 412)]),
    monokulo: report('boot-1', [sample(0, 2, 96)]),
    one_process: false,
  }, { node: node, scaling: scaling('link') }));
  assert.deepEqual(view.tiles.map((t) => t.key), ['cpu', 'memory', 'transfer', 'round-trip', 'first-byte', 'block-size']);
  assert.ok(view.stacked);
  assert.equal(view.scanning?.preview, '14 behind · pace: link speed');
  assert.equal(view.at_unix, NOW);
});

test('figures read as people write them', () => {
  assert.equal(bytes(412_000_000), '412 MB');
  assert.equal(bytes(1_800_000), '1.8 MB');
  assert.equal(bytes(13_000), '13.0 kB');
  assert.equal(bytes(900), '900 B');
  assert.equal(bytes(7_600_000_000), '7.6 GB');
  assert.equal(rate(387_500), '3.1 Mbit/s');
  assert.equal(rate(60_000), '480 kbit/s');
  assert.equal(duration(130), '2 m 10 s');
  assert.equal(duration(45), '45 s');
  assert.equal(duration(3_900), '1 h 5 m');
  assert.equal(durationRough(1_080), '18 minutes');
  assert.equal(durationRough(40), '40 seconds');
  assert.equal(durationRough(60), '60 seconds');
  assert.equal(durationRough(7_200), '2 hours');
  assert.equal(thousands(3_412_001), '3,412,001');
  assert.equal(thousands(999), '999');
  assert.equal(thousands(1_000), '1,000');
});
