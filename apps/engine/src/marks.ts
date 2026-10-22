// Which marks the events table lists: those due by the playback position,
// newest first. Marks come oldest first, their times in the order the engine
// recorded them, so the due ones are found by halving rather than by reading
// them all.
import type { Mark, Tier } from './wire.ts';

/** How many of `marks` (oldest first) are due by `at`: the first that many. */
export function dueCount(marks: readonly Mark[], at: number): number {
  let [low, high] = [0, marks.length];
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (marks[middle].at_ms <= at) low = middle + 1;
    else high = middle;
  }
  return low;
}

/** The newest `limit` of the first `due` marks whose tier is shown, newest
 *  first. */
export function newestRows(marks: readonly Mark[], due: number, tiers: ReadonlySet<Tier>, limit: number): Mark[] {
  const rows: Mark[] = [];
  for (let i = due - 1; i >= 0 && rows.length < limit; i--) {
    if (tiers.has(marks[i].tier)) rows.push(marks[i]);
  }
  return rows;
}
