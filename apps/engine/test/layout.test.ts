// How the labels above the chain strip's blocks stack.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { stackLabels } from '../src/layout.ts';

const cases: { case: string; spans: [number, number][]; levels: number[]; used: number }[] =
  JSON.parse(readFileSync(new URL('./label_levels.json', import.meta.url), 'utf8'));

test('labels stack as few levels high as they can, each clear of those on its level', () => {
  assert.ok(cases.length >= 7);
  for (const c of cases) assert.deepEqual(stackLabels(c.spans), [c.levels, c.used], c.case);
});
