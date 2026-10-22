// The engine page's events table: the newest marks up to the playback
// position, those of the tiers ticked, newest first. It is worked out from
// four signals, and nothing redraws it by hand: the stream sets `marks`,
// the playback loop `head`, the playback control `live` and the tier
// checkboxes `tiers`.
import { createMemo, createSignal, For } from 'solid-js';
import { render } from '@solidjs/web';

/** One mark, as the server sends it (`engine_view::present::MarkView`). */
export interface Mark {
  seq: number;
  at_ms: number;
  round: number;
  tier: string;
  key: boolean;
  text: string;
  /** When it reached the page (`performance.now()`); absent for the history. */
  arrived?: number;
}

const ROWS = 60;
const TIER_NAMES: Record<string, string> = { chain: 'Chain', blocks: 'Blocks', mempool: 'Mempool', settlement: 'Settlement', upkeep: 'Upkeep' };

// The marks change in place, oldest first: every write is news.
const [marks, setMarks] = createSignal<readonly Mark[]>([], { equals: false });
const [head, setHead] = createSignal(0);
const [live, setLive] = createSignal(true);
const [tiers, setTiers] = createSignal<ReadonlySet<string>>(new Set(Object.keys(TIER_NAMES)));
export { setMarks, setHead, setLive, setTiers };

const fmt = (n: number) => n.toLocaleString('en-GB');
const same = (a: readonly Mark[], b: readonly Mark[]) => a.length === b.length && a.every((mark, i) => mark === b[i]);

function Rows() {
  // The playback position moves every frame; the rows change only when a
  // mark comes due, a tier is ticked, or the marks themselves change.
  const rows = createMemo(() => {
    const all = marks(), at = head(), shown = tiers(), found: Mark[] = [];
    for (let i = all.length - 1; i >= 0 && found.length < ROWS; i--) {
      const mark = all[i];
      if (mark.at_ms <= at && (!mark.tier || shown.has(mark.tier))) found.push(mark);
    }
    return found;
  }, { equals: same });
  return (
    <For each={rows()} fallback={<tr><td colspan="3"><span class="engine-hint">Nothing recorded yet.</span></td></tr>}>
      {(mark, i) => (
        <tr data-key={mark.key ? '' : undefined} data-now={i() === 0 && !live() ? '' : undefined} data-at={mark.at_ms}>
          <td>{fmt(mark.round)}</td>
          <td>{mark.tier ? <span class={`tierchip t-${mark.tier}`}>{TIER_NAMES[mark.tier]}</span> : null}</td>
          <td>{mark.text}</td>
        </tr>
      )}
    </For>
  );
}

/** Takes over the table's body from the server's rows, once the page has
 *  the history to draw it from. */
export function mountEvents(body: HTMLTableSectionElement): () => void {
  body.replaceChildren();
  return render(() => <Rows />, body);
}
