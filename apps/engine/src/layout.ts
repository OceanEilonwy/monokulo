// Where the chain strip puts things, from its measured width: which blocks
// fit, and how its labels stack. Pure, so the unit tests check them
// (test/layout.test.ts, against label_levels.json).

import type { ChainView } from './present.ts';

/** The chain strip's cells keep their size: 22px and a 4px gap, the next
 *  block 32px; the strip's padding is 14px left and 8px right. */
export const CELL_PX = 26;
export const NEXT_PX = 32;
export const STRIP_PAD_PX = 22;
/** Narrower than this many cells, the strip shows the newest blocks only,
 *  without the cut back to the lowest group of stores. */
export const CUT_FROM_CELLS = 20;
/** The space kept between two labels on one level, in pixels. */
export const LABEL_GAP_PX = 4;

/** As many whole cells as a strip `width` pixels wide holds, the next
 *  block's wider one among them: a wider window shows more blocks, never
 *  wider ones. */
export function cellsThatFit(width: number): number {
  return Math.max(1, 1 + Math.floor((width - STRIP_PAD_PX - NEXT_PX) / CELL_PX));
}

/** The blocks to draw in `cells` cells, oldest first, null for the cut (as
 *  wide as three cells). The newest is the block still to come, at the right
 *  edge. */
export function visibleBlocks(chain: ChainView, cells: number): (number | null)[] {
  if (chain.tip === null || chain.high_water === null) return [];
  const right = Math.max(chain.tip, chain.high_water) + 1;
  // The cut's end moves in steps of five, rather than with each block
  // arriving during initial node synchronisation.
  const lowest = Math.floor(Math.min(chain.lowest ?? chain.high_water, chain.high_water) / 5) * 5;
  const all: (number | null)[] = [];
  const left = Math.max(0, right - cells + 1);
  if (cells < CUT_FROM_CELLS || lowest - 1 >= left) {
    for (let h = left; h <= right; h++) all.push(h);
    return all;
  }
  for (let h = Math.max(0, lowest - 1); h <= lowest + 5; h++) all.push(h);
  all.push(null);
  for (let h = right - (cells - 11); h <= right; h++) all.push(h);
  return all;
}

/** Each label's level, given its left and right edges in pixels, in the
 *  order given (the node's tip, the high-water mark, then each group of
 *  stores): each goes on the lowest level where it is `LABEL_GAP_PX` clear
 *  of every label already there. Returns the levels and how many are used
 *  (at least one). */
export function stackLabels(spans: [number, number][]): [number[], number] {
  const placed: [number, number][][] = [];
  const levels = spans.map(([left, right]) => {
    let level = placed.findIndex((on) => on.every(([l, r]) => r + LABEL_GAP_PX <= left || right + LABEL_GAP_PX <= l));
    if (level < 0) {
      level = placed.length;
      placed.push([]);
    }
    placed[level].push([left, right]);
    return level;
  });
  return [levels, Math.max(1, placed.length)];
}
