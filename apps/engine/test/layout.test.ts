// Where the chain strip puts things: which blocks a width holds, and how the
// labels above them stack.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { CUT_FROM_CELLS, cellsThatFit, stackLabels, visibleBlocks } from '../src/layout.ts';
import type { ChainView } from '../src/present.ts';

const cases: { case: string; spans: [number, number][]; levels: number[]; used: number }[] =
  JSON.parse(readFileSync(new URL('./label_levels.json', import.meta.url), 'utf8'));

test('labels stack as few levels high as they can, each clear of those on its level', () => {
  assert.ok(cases.length >= 7);
  for (const c of cases) assert.deepEqual(stackLabels(c.spans), [c.levels, c.used], c.case);
});

const chain = (tip: number, high: number, lowest: number | null): ChainView => ({
  tip, high_water: high, lowest, window_from: null, cached: [], checkpoints: [], replaced: [], scanning: null,
  groups: [], next_block: null, cache: '', nodes: [], call: null,
});

test('a wider strip holds more whole cells, the next block wider among them', () => {
  // 22px of padding, the next block 32px, every other cell 26px with its gap.
  assert.equal(cellsThatFit(22 + 32), 1);
  assert.equal(cellsThatFit(22 + 32 + 25), 1);
  assert.equal(cellsThatFit(22 + 32 + 26), 2);
  assert.equal(cellsThatFit(800), 1 + Math.floor((800 - 54) / 26));
  assert.equal(cellsThatFit(0), 1, 'always the next block');
});

test('the newest blocks fill the strip, the block still to come at the right edge', () => {
  const blocks = visibleBlocks(chain(1_000, 1_000, 998), 10);
  assert.deepEqual(blocks, [992, 993, 994, 995, 996, 997, 998, 999, 1_000, 1_001]);
  assert.deepEqual(visibleBlocks({ ...chain(0, 0, null), tip: null }, 10), [], 'nothing known: nothing drawn');
});

test("stores far behind keep their blocks in view across a cut, on a strip wide enough", () => {
  const cells = 40;
  const blocks = visibleBlocks(chain(1_000, 1_000, 512), cells);
  const cut = blocks.indexOf(null);
  assert.ok(cut > 0, 'a cut');
  // The lowest group's block taken down to a step of five (510), with one
  // before it and five after.
  assert.deepEqual(blocks.slice(0, cut), [509, 510, 511, 512, 513, 514, 515]);
  assert.equal(blocks[blocks.length - 1], 1_001, 'the block still to come at the right edge');
  // The cut is as wide as three cells: the blocks and it fill the strip.
  assert.equal(blocks.length - 1 + 3, cells);
  // Narrower than CUT_FROM_CELLS, only the newest.
  const narrow = visibleBlocks(chain(1_000, 1_000, 512), CUT_FROM_CELLS - 1);
  assert.ok(!narrow.includes(null));
  assert.equal(narrow[narrow.length - 1], 1_001);
});
