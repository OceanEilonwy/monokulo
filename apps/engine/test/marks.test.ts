// Which marks the events table lists, found without reading them all.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { dueCount, newestRows } from '../src/marks.ts';
import { markText } from '../src/present.ts';
import { TIERS } from '../src/wire.ts';
import type { Mark, Tier } from '../src/wire.ts';

const mark = (seq: number, at_ms: number, tier: Tier): Mark => ({
  seq, at_ms, round: 1, tier, key: false, what: { kind: 'pruned', hashes: seq },
});

// Issue #126: a hidden tab keeps only its newest frame (engine-view.tsx), so
// a mark can come in a frame that is never played. The coverage fixture's
// reorg story ends with "Rewound", then round 4's frames, which carry no
// marks: shown again, the page plays only the last of those, and the table
// still lists the reorg's last mark, because it lists the marks the page
// holds that are due by the playback position, whichever frames played.
test('a mark in a frame a hidden tab never played is listed once the playback passes it', () => {
  const rewound: Mark = { seq: 41, at_ms: 1_000, round: 3, tier: 'chain', key: true,
    what: { kind: 'rewound', fork: 3_412_881, deleted_to: null, ancestor: 3_412_880 } };
  const frames = [
    { at_ms: 1_000, marks: [mark(40, 880, 'blocks'), rewound] },
    { at_ms: 1_120, marks: [] },
    { at_ms: 1_240, marks: [] },
  ];
  // Every frame's marks are held as it arrives; hidden, only the newest
  // frame is kept to play.
  const held = frames.flatMap((frame) => frame.marks);
  const played = frames.slice(-1)[0];
  const rows = newestRows(held, dueCount(held, played.at_ms), new Set(TIERS), 60);
  assert.equal(rows[0], rewound);
  assert.ok(markText(rows[0].what).startsWith('Rewound: block 3,412,881 deleted'), markText(rows[0].what));
});

test('fifty thousand marks: the due ones are found by halving', () => {
  const many = Array.from({ length: 50_000 }, (_, i) => mark(i, i * 30, 'blocks'));
  assert.equal(dueCount(many, 30 * 25_000), 25_001);
  assert.equal(dueCount(many, -1), 0);
  assert.equal(dueCount(many, Infinity), 50_000);
});
