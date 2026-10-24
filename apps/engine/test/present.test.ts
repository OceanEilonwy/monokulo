// What the page says for states the machine reaches: each scenario's state as
// monokulo sends it (fixtures.json, checked against the machine by monokulo's
// engine_view::app_fixtures).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import type { Presented } from '../src/present.ts';
import { markText, present } from '../src/present.ts';
import type { MarkKind, State, Tuning } from '../src/wire.ts';

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
