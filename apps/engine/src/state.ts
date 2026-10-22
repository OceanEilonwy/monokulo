// What the engine page's views are drawn from. The controller
// (engine-view.ts) writes these: the stream and the playback loop, the
// timeline and the controls. The views (*.tsx) only read them, so each
// redraws exactly when what it shows changes.

import { createSignal } from 'solid-js';
import type { MachineView } from './machine.ts';
import type { Presented, RibbonMark, RoundView } from './present.ts';
import type { Mark, Tier } from './wire.ts';
import { TIERS } from './wire.ts';

/** The network the page follows. */
export const [network, setNetwork] = createSignal('');
/** The state at the playback position, in words and figures; null until the
 *  first history arrives. */
export const [view, setView] = createSignal<Presented | null>(null);
/** The "Machine and links" strip and the Scanning panel; null until the
 *  first `machine` event. */
export const [machine, setMachine] = createSignal<MachineView | null>(null);
/** How long ago the engine reported the machine's figures: "live · 3 s ago". */
export const [machineAge, setMachineAge] = createSignal<string | null>(null);
/** Every mark the page holds, oldest first. Changed in place: every write is
 *  news. */
export const [marks, setMarks] = createSignal<readonly Mark[]>([], { equals: false });
/** The playback position, engine milliseconds. */
export const [head, setHead] = createSignal(0);
/** Playing live (not paused or replaying). */
export const [live, setLive] = createSignal(true);
/** The tiers ticked in the events table's filter. */
export const [tiers, setTiers] = createSignal<ReadonlySet<Tier>>(new Set(TIERS));
/** A past round chosen from the recent rounds, shown in the round card in
 *  place of the live one, with the recent rounds held as they were. */
export const [pinned, setPinned] = createSignal<{ round: RoundView; ribbon: RibbonMark[] } | null>(null);
/** The chain strip's width in pixels: how many blocks it shows. */
export const [stripWidth, setStripWidth] = createSignal(0);
/** The scan is catching up thousands of blocks: animations are left out. */
export const [catchingUp, setCatchingUp] = createSignal(false);
