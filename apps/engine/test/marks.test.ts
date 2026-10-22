// Which marks the events table lists, found without reading them all.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dueCount, newestRows } from '../src/marks.ts';
import type { Mark, Tier } from '../src/wire.ts';

const mark = (seq: number, at_ms: number, tier: Tier): Mark => ({
  seq, at_ms, round: 1, tier, key: false, what: { kind: 'pruned', hashes: seq },
});
const marks = [mark(1, 100, 'blocks'), mark(2, 200, 'upkeep'), mark(3, 200, 'blocks'), mark(4, 300, 'chain'), mark(5, 400, 'blocks')];

test('the marks due by a moment: those at it or before', () => {
  assert.equal(dueCount(marks, 0), 0);
  assert.equal(dueCount(marks, 100), 1);
  assert.equal(dueCount(marks, 199), 1);
  assert.equal(dueCount(marks, 200), 3, 'both at the moment');
  assert.equal(dueCount(marks, 1_000), 5);
  assert.equal(dueCount([], 1_000), 0);
});

test('the rows: the newest due marks of the tiers shown, newest first, at most the limit', () => {
  const all = new Set<Tier>(['chain', 'blocks', 'mempool', 'settlement', 'upkeep']);
  assert.deepEqual(newestRows(marks, 4, all, 60).map((m) => m.seq), [4, 3, 2, 1]);
  assert.deepEqual(newestRows(marks, 5, all, 2).map((m) => m.seq), [5, 4]);
  assert.deepEqual(newestRows(marks, 5, new Set<Tier>(['upkeep']), 60).map((m) => m.seq), [2]);
  assert.deepEqual(newestRows(marks, 0, all, 60), []);
});

test('fifty thousand marks: the due ones are found by halving', () => {
  const many = Array.from({ length: 50_000 }, (_, i) => mark(i, i * 30, 'blocks'));
  assert.equal(dueCount(many, 30 * 25_000), 25_001);
  assert.equal(dueCount(many, -1), 0);
  assert.equal(dueCount(many, Infinity), 50_000);
});
