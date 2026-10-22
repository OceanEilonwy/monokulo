// What the page says for states the machine reaches: each scenario's state as
// monokulo sends it (fixtures.json, checked against the machine by monokulo's
// engine_view::app_fixtures).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { compact, laneTime, seconds } from '../src/format.ts';
import type { Piece, Presented, Shape } from '../src/present.ts';
import { markText, meets, outcomeChip, present, ribbonHeight, shapes, tierName } from '../src/present.ts';
import type { MarkKind, State, Tuning } from '../src/wire.ts';
import { TIERS } from '../src/wire.ts';

const fixtures: { tuning: Tuning; states: Record<string, State>; marks: MarkKind[] } =
  JSON.parse(readFileSync(new URL('./fixtures.json', import.meta.url), 'utf8'));
const TUNING = fixtures.tuning;
const presented = (scenario: string): Presented => {
  const state = fixtures.states[scenario];
  assert.ok(state, `no scenario ${scenario}`);
  return present(state, TUNING);
};
const figure = (p: Presented, label: string) => {
  const found = p.summary.find((f) => f.label === label);
  assert.ok(found, `no ${label}`);
  return [found.value, found.note];
};

test('before anything is known, the page says so instead of inventing zeros', () => {
  const p = presented('empty');
  assert.deepEqual(figure(p, 'Node tip'), ['–', 'no node reported yet']);
  assert.deepEqual(figure(p, 'Behind'), ['–', 'not known yet']);
  assert.equal(figure(p, 'Last round')[0], '–');
  assert.equal(figure(p, 'Chain')[0], '–');
  assert.equal(p.round, null);
  assert.equal(p.chain.call, null, 'no call seen yet');
  assert.equal(p.side.reorg.summary, 'No reorganisation');
});

test('a caught-up network reads as caught up', () => {
  const p = presented('caught_up');
  assert.deepEqual(figure(p, 'Node tip'), ['3,412,880', 'node-a']);
  assert.equal(figure(p, 'Scanned to')[0], '3,412,880');
  assert.deepEqual(figure(p, 'Behind'), ['0', 'caught up']);
  assert.deepEqual(figure(p, 'Stores'), ['41', 'all at the high-water mark']);
  assert.deepEqual(figure(p, 'Chain'), ['Agrees', 'with the node']);
  assert.equal(p.chain.window_from, 3_412_861, 'the last 20 blocks');
  assert.equal(p.chain.groups.length, 1);
  assert.ok(p.chain.groups[0].frontier);
  assert.equal(p.chain.groups[0].label, 'Frontier, 41 stores');
  assert.equal(p.chain.groups[0].title, '41 stores whose scan has reached block 3,412,880');
  assert.equal(p.chain.cache, 'cache 3.2 MB of 64.0 MB');
  assert.equal(p.chain.call, 'on_get_block_hash 3,412,880');
  assert.deepEqual(p.chain.nodes.map((n) => [n.chip, n.tone]), [['active', 'ok'], ['cooling down', 'warn']]);
  assert.equal(p.side.reorg.summary, 'Agrees with the node at 3,412,880');
  assert.equal(p.side.database.summary, '3 queued, 1,234 jobs done');
  assert.deepEqual(p.side.database.rows.find(([label]) => label === 'Longest wait for a turn'), ['Longest wait for a turn', '1.7ms']);
  assert.deepEqual(p.side.database.rows.find(([label]) => label === 'Longest job'), ['Longest job', '340µs']);
});

test('stores catching up read as catching up, a big group in pages, the busy one the one being scanned', () => {
  const p = presented('catching_up');
  assert.deepEqual(figure(p, 'Behind'), ['42', '3 stores catching up']);
  assert.deepEqual(figure(p, 'Stores'), ['603', '3 catching up']);
  const groups = p.chain.groups;
  assert.equal(groups[0].label, 'Frontier, 600 stores (3 pages)');
  assert.ok(!groups[0].busy);
  assert.equal(groups[1].label, 'Catching up, 3 stores');
  assert.ok(groups[1].busy);
  assert.deepEqual(p.chain.scanning, [959, 0.25]);
  assert.equal(p.chain.lowest, 958);
});

test("a round's lanes are drawn to scale: its length with a floor, or its budget while stores catch up", () => {
  const round = presented('round_quick').round!;
  assert.equal(round.title, 'Round 1,290');
  assert.equal(round.state, 'Running.', "the page's first round");
  assert.equal(round.elapsed, '62ms');
  assert.equal(round.scale_ms, 71, 'the round and 15 % more');
  const blocks = round.lanes[1];
  assert.deepEqual([blocks.name, blocks.share], ['Blocks', '40 %']);
  assert.equal(blocks.bars.length, 1, 'pass 1 and pass 2 back to back: one segment');
  assert.deepEqual(blocks.bars[0].shapes, [
    { kind: 'fill', from_ms: 2, to_ms: 42, leftover: false },
    { kind: 'fill', from_ms: 42, to_ms: 62, leftover: true },
  ], 'pass 2 striped after pass 1');
  assert.equal(blocks.bars[0].label, '60ms');
  assert.equal(blocks.reserved, null);
  assert.equal(round.lanes[0].outcome!.text, 'Idle');
  assert.deepEqual(round.lanes[2].outcome, { tone: 'warn', text: "Waiting: the pool couldn't be read" });
  assert.equal(round.lanes[3].outcome, null, 'not ended yet');

  const budget = presented('round_to_budget').round!;
  assert.equal(budget.scale_ms, 10_000);
  assert.deepEqual(budget.lanes[1].reserved, [2, 4_000]);
  assert.deepEqual(budget.lanes[0].reserved, [0, 2_000]);
  assert.equal(budget.lanes[4].reserved, null, "upkeep hasn't started");
});

test("a round's parts add up to it, and the end marker is on the segment that finished last", () => {
  const round = presented('round_parts').round!;
  assert.equal(round.lanes.reduce((sum, lane) => sum + lane.ms, 0), round.elapsed_ms, 'the parts add up to the round');
  assert.deepEqual([round.elapsed_ms, round.elapsed], [466, '466ms']);
  const drawn = (lane: number) => round.lanes[lane].bars.map((bar) => [bar.start_ms, bar.ms, bar.label, bar.last]);
  assert.deepEqual(drawn(0), [[0, 423, '423ms', false]], 'one Chain segment');
  assert.equal(round.lanes[0].bars[0].title, 'Chain: 423 ms total · 2 operations');
  assert.ok(!round.lanes[0].bars[0].work, 'not only the tip request');
  assert.deepEqual(round.lanes[0].bars[0].shapes, [
    { kind: 'work', from_ms: 0, to_ms: 403, joined: false, band_from: null },
    { kind: 'fill', from_ms: 403, to_ms: 423, leftover: false },
  ], 'the tip request outlined, then the unit solid');
  assert.deepEqual(drawn(1), [[423, 42, '42ms', true]], 'the unit and the cache carry after it are one segment; it finished last');
  const blocks = round.lanes[1].bars[0];
  assert.equal(blocks.span_ms, 43);
  assert.deepEqual(blocks.shapes, [
    { kind: 'fill', from_ms: 423, to_ms: 463, leftover: false },
    { kind: 'work', from_ms: 463, to_ms: 466, joined: true, band_from: null },
  ], '1ms between them, well under the threshold: the outline starts where the unit ends');
  assert.deepEqual(drawn(2), [[463, 1, '1ms', false]], 'the 0ms span left out');
  assert.deepEqual(drawn(3), [[464, 0, '<1ms', false]], 'a lane with only 0ms keeps one, under a millisecond');
  assert.deepEqual(drawn(4), []);
  const labelled = round.lanes.flatMap((lane) => lane.bars).map((bar) => bar.label)
    .filter((label): label is string => label !== null)
    .reduce((sum, label) => sum + (label === '<1ms' ? 0 : Number(label.replace('ms', ''))), 0);
  assert.equal(labelled, 466, 'the labels add up to the round');
  assert.equal(round.state, 'Ended. Sleeping until the poll interval is up or the node announces a block.');
});

test("a round's state line says what woke it and how it ended", () => {
  const line = (scenario: string) => presented(scenario).round!.state;
  assert.equal(line('round_running'), 'Running.');
  assert.equal(line('round_after_interval'), 'Running, after the poll interval.');
  assert.equal(line('round_woken'), 'Running, woken by the node announcing a block.');
  assert.equal(line('round_at_once'), 'Running, started at once: work was left.');
  assert.equal(line('round_ended_backlogged'), 'Ended with work left: the next round starts at once.');
  assert.equal(line('round_ended'), 'Ended. Sleeping until the poll interval is up or the node announces a block.');
});

test('the ribbon shows rounds by length on a log scale and sleeps by what ended them', () => {
  const ribbon = presented('ribbon').ribbon;
  assert.deepEqual(ribbon[0], { kind: 'round', number: 9, height: 22, parts: [['blocks', 0.75], ['settlement', 0.25]], title: 'Round 9: 1.0s' });
  assert.deepEqual(ribbon[1], { kind: 'sleep', woken: true, title: 'Slept 0.30s, cut short by a new block' });
  assert.equal(ribbonHeight(0), 4);
  assert.equal(ribbonHeight(1), 4);
  assert.equal(ribbonHeight(10_000), 28);
  assert.equal(ribbonHeight(600_000), 28);
});

test('an open reorg draws attention, and a diverged chain too', () => {
  const p = presented('reorg_open');
  assert.deepEqual(figure(p, 'Chain'), ['Reorg', 'from block 99']);
  const reorg = p.side.reorg;
  assert.ok(reorg.alert);
  assert.equal(reorg.summary, 'Reorg from 99: re-examining payments');
  assert.deepEqual(reorg.rows.find(([label]) => label === 'Re-examined, changed, voided'), ['Re-examined, changed, voided', '3, 1, 0']);
  assert.equal(p.side.reorg_step, 'process');
  assert.ok(p.chain.groups.every((g) => g.waiting));
  assert.equal(p.side.orders.summary, 'paid held while the reorg is open');
  assert.deepEqual(p.chain.replaced, [99, 100]);

  const diverged = presented('diverged');
  assert.deepEqual(figure(diverged, 'Chain'), ['Diverged', 'waiting for reconciliation']);
  assert.ok(diverged.side.reorg.alert);
});

test('the side summarises the pool, orders, upkeep and saves', () => {
  const side = presented('side').side;
  assert.equal(side.mempool.summary, '12 scanned in the pool, 1 payment found');
  assert.deepEqual(side.pool_txs, [['aaaaaaaa', true]]);
  assert.equal(side.orders.summary, '2 to recompute');
  assert.deepEqual(side.transitions, [['pending', 'unconfirmed']]);
  assert.equal(side.upkeep.summary, 'ran in round 77');
  assert.equal(side.restart.summary, '2 saves a minute; 0 blocks only in memory');
});

test('seconds read to two places under one and one over; chips have their tones', () => {
  assert.equal(seconds(0), '0.00s');
  assert.equal(seconds(420), '0.42s');
  assert.equal(seconds(999), '1.00s');
  assert.equal(seconds(1_000), '1.0s');
  assert.equal(seconds(7_840), '7.8s');
  assert.equal(outcomeChip('failed').tone, 'err');
  assert.equal(outcomeChip('backlogged').tone, 'hi');
  for (const tier of TIERS) assert.ok(tierName(tier));
});

test("the next block fills with the node's pool, and says when it holds more than a block", () => {
  assert.equal(presented('empty').chain.next_block, null, 'not asked yet');
  const quarter = presented('pool_quarter').chain.next_block!;
  assert.equal(quarter.fill, 0.25);
  assert.ok(!quarter.over);
  assert.equal(quarter.count, '23');
  assert.equal(quarter.title, "The next block: 23 transactions waiting in the node's pool, 75 kB of the 300 kB a miner can fill at full reward (25 %).");
  const full = presented('pool_full').chain.next_block!;
  assert.deepEqual([full.fill, full.over, full.count], [1, true, '1.2k']);
  assert.ok(full.title.endsWith('(150 %): more than one block takes without a smaller reward.'));
  const sizeless = presented('pool_sizeless').chain.next_block!;
  assert.equal(sizeless.fill, null);
  assert.equal(sizeless.title, "The next block: 1 transaction waiting in the node's pool (the node didn't say their size).");
  assert.equal(compact(999), '999');
  assert.equal(compact(12_345), '12k');
  assert.equal(compact(2_500_000), '2.5M');
});

test('the pool panel says whether the engine looks at the pool', () => {
  const mempool = (scenario: string) => presented(scenario).side.mempool;
  const idle = mempool('pool_unwatched');
  assert.equal(idle.summary, 'Not scanned: no order waits to be paid');
  assert.deepEqual(idle.rows[0], ["In the node's pool", '–']);
  assert.deepEqual(idle.rows[2], ['Scanned by the engine', 'no: no order waits to be paid']);
  assert.equal(mempool('pool_unwatched_node').summary, "30 in the node's pool, not scanned");
  const looking = mempool('pool_watched_node');
  assert.equal(looking.summary, "30 in the node's pool");
  assert.equal(looking.rows[1][1], '20 %');
  assert.equal(looking.rows[2][1], 'yes');
  assert.equal(mempool('pool_watched').summary, '4 scanned in the pool');
});

test('many short round operations have a compact tooltip and aggregated details', () => {
  const bar = presented('many_operations').round!.lanes[1].bars[0];
  assert.equal(bar.ms, 1000);
  assert.ok(bar.title.length < 100);
  assert.ok(bar.title.includes('1000 operations'));
  assert.deepEqual(bar.details, ['Block scan work: 1000, 1,000 ms total']);
});

const piece = (start_ms: number, ms: number, solid: boolean): Piece => ({ start_ms, ms, solid, leftover: false });
const fill = (from_ms: number, to_ms: number, leftover = false): Shape => ({ kind: 'fill', from_ms, to_ms, leftover });
const work = (from_ms: number, to_ms: number, joined: boolean, band_from: number | null): Shape => ({ kind: 'work', from_ms, to_ms, joined, band_from });
const thread = (from_ms: number, to_ms: number, band: boolean): Shape => ({ kind: 'thread', from_ms, to_ms, band });

test('a gap is joined, banded or crossed by a thread by its share of the scale', () => {
  const at = (gap: number) => shapes([piece(100, 300, true), piece(400 + gap, 40, false)], 1_000);
  assert.deepEqual(at(0), [fill(100, 400), work(400, 440, true, null)], 'back to back');
  assert.deepEqual(at(24), [fill(100, 400), work(400, 464, true, null)], '2.4 %: joined, the outline covering the gap');
  assert.deepEqual(at(25), [fill(100, 400), thread(400, 425, true), work(425, 465, false, 400)], '2.5 %: split, but a band a narrow track joins');
  assert.deepEqual(at(49), [fill(100, 400), thread(400, 449, true), work(449, 489, false, 400)], '4.9 %: still a band');
  assert.deepEqual(at(50), [fill(100, 400), thread(400, 450, false), work(450, 490, false, null)], '5 %: split everywhere');
});

test('joined work merges into one outline, and a segment can be split after a join', () => {
  assert.deepEqual(
    shapes([piece(0, 200, true), piece(200, 30, false), piece(235, 10, false), piece(245, 5, false)], 1_000),
    [fill(0, 200), work(200, 250, true, null)], 'three pieces, gaps of 0, 5 and 0: one outline');
  assert.deepEqual(
    shapes([piece(80, 250, true), piece(330, 40, false), piece(520, 36, false)], 710),
    [fill(80, 330), work(330, 370, true, null), thread(370, 520, false), work(520, 556, false, null)],
    'solid, joined outline, thread, outline');
  assert.deepEqual(shapes([piece(0, 80, false)], 710), [work(0, 80, false, null)], 'work alone: an outline alone');
});

test('shapes back to back meet, and a gap or a thread keeps them apart', () => {
  assert.ok(meets(work(0, 40, false, null), fill(40, 43)), 'hollow, solid');
  assert.ok(meets(fill(0, 40), work(40, 50, true, null)), 'solid, hollow');
  assert.ok(meets(fill(0, 40), fill(40, 60, true)), 'solid, solid');
  assert.ok(meets(work(0, 40, false, null), work(40, 50, false, null)), 'hollow, hollow');
  const chain = [work(0, 40, false, null), fill(40, 80), fill(80, 90, true), work(90, 95, true, null)];
  assert.ok(chain.slice(1).every((next, i) => meets(chain[i], next)), 'a chain of four');
  assert.ok(!meets(fill(0, 40), work(41, 50, false, null)), 'a millisecond apart');
  assert.ok(!meets(work(0, 40, false, null), fill(0, 40)), 'the next one first');
  assert.ok(!meets(fill(0, 40), thread(40, 60, false)), 'a thread is a line');
  assert.ok(!meets(thread(40, 60, false), work(60, 70, false, null)), 'a thread is a line');
});

test('a very short round is drawn to a 10ms scale, and a part under a millisecond says so', () => {
  const round = presented('very_short_round').round!;
  assert.equal(round.scale_ms, 10, 'the floor');
  assert.equal(round.elapsed, '2ms');
  const blocks = round.lanes[1].bars[0];
  assert.deepEqual(blocks.shapes, [fill(1, 2), thread(2, 3, false), work(3, 3, false, null)],
    '1ms is 10 % of the scale: split, the carry an outline under a millisecond');
  assert.equal(blocks.label, '1ms');
  const upkeep = round.lanes[4].bars[0];
  assert.equal(upkeep.label, '<1ms', 'never 0ms');
  assert.ok(upkeep.last, 'the marker on the segment that finished last');
  assert.equal(laneTime(0), '<1ms');
  assert.equal(laneTime(1_200), '1,200ms');
});

test('every mark reads as a sentence, each as the machine sends it', () => {
  const sentences = [
    'The node has a new block: 101.',
    'The node has 3 new blocks, up to 104.',
    'The settlement tier failed and stopped for this round; it is retried next round.',
    'The blocks tier stopped for this round: the node failed.',
    'Block 101 scanned for 4 stores and committed.',
    'Block 101 scanned for 1 store and committed, 1 payment found in it.',
    'Block 101 scanned for 3 stores and committed, 40 payments found in it.',
    'Block 101 recorded from its header: no store had anything to look for in it.',
    'Block 51 scanned for 3 stores catching up.',
    'Block 100 scanned for 2 stores catching up: they caught up and joined the frontier.',
    '1 store with nothing that could have been paid moved straight from block 30 to 100.',
    'Out of time partway through block 7: 1,200 of 3,000 transactions scanned for 2 stores, saved.',
    'Started scanning this network at block 3,412,879.',
    "The node's chain differs from block 99 on: a reorganisation. Payments from there are re-examined, and new blocks wait.",
    'The reorganisation goes deeper: it is reconciled from block 98 now.',
    'Payments at or above the fork were queued to be re-examined.',
    "Re-examined 2 payments against the node's chain: 1 changed, 0 voided as double-spent.",
    'Rewound: blocks 98 to 100 deleted, stores moved back to block 97. The replacement blocks are scanned next.',
    'Rewound: block 100 deleted, stores moved back to block 99. The replacement block is scanned next.',
    "Block 101 from the node doesn't extend the recorded chain: new blocks wait for the reorganisation to be reconciled.",
    'The fast path scanned 2 transactions new in the pool.',
    'Transaction deadbeef in the pool pays an order: recorded at once by the fast path.',
    "Transaction deadbeef in the pool pays an order: found by the round's rotation.",
    'An order went from pending to unconfirmed; its order event is written.',
    '4 orders changed status (one from confirming to paid); their order events are written.',
    'Pruned 1,200 old block hashes.',
  ];
  assert.equal(fixtures.marks.length, sentences.length);
  fixtures.marks.forEach((mark, i) => assert.equal(markText(mark), sentences[i], JSON.stringify(mark)));
  // Every kind there is, at least once.
  const kinds = new Set(fixtures.marks.map((mark) => mark.kind));
  assert.equal(kinds.size, 16);
});
