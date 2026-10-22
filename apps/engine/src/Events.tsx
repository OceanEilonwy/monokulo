// The events table: the newest marks up to the playback position, those of
// the tiers ticked, newest first, a click going to the moment of one.
import { createMemo, For } from 'solid-js';
import { Cell, Checkbox, Row, Table } from '@monokulo/ui/solid';
import { thousands } from './format.ts';
import { dueCount, newestRows } from './marks.ts';
import { markText, tierName } from './present.ts';
import { head, live, marks, setTiers, tiers } from './state.ts';
import type { Mark } from './wire.ts';
import { TIERS } from './wire.ts';

const ROWS = 60;
const same = (a: readonly Mark[], b: readonly Mark[]) => a.length === b.length && a.every((mark, i) => mark === b[i]);

export function Events(props: { onPick: (atMs: number) => void }) {
  // The playback position moves every frame: how many marks are due follows
  // it cheaply, and the rows change only when a mark comes due, a tier is
  // ticked, or the marks themselves change.
  const due = createMemo(() => dueCount(marks(), head()));
  const rows = createMemo(() => newestRows(marks(), due(), tiers(), ROWS), { equals: same });
  const tick = (tier: (typeof TIERS)[number], checked: boolean) => {
    const next = new Set(tiers());
    if (checked) next.add(tier);
    else next.delete(tier);
    setTiers(next);
  };
  return (
    <section class="engine-card" aria-labelledby="h-events">
      <header>
        <h2 id="h-events">Events</h2>
        <span class="engine-hint">newest first, up to the playback position; a click goes there</span>
        <div class="engine-filters engine-right" id="engine-filters">
          <For each={TIERS}>
            {(tier) => (
              <Checkbox
                text={<span class={`tierchip t-${tier}`}>{tierName(tier)}</span>}
                checked={true}
                data-tier={tier}
                onChange={(event: Event) => tick(tier, (event.currentTarget as HTMLInputElement).checked)}
              />
            )}
          </For>
        </div>
      </header>
      <div class="engine-events">
        <Table
          id="engine-events"
          scroll={false}
          head={<Row><Cell header={true}>Round</Cell><Cell header={true}>Tier</Cell><Cell header={true}>What happened</Cell></Row>}
        >
          <For each={rows()} fallback={<Row><Cell colspan="3"><span class="engine-hint">Nothing recorded yet.</span></Cell></Row>}>
            {(mark, i) => (
              <Row
                data-key={mark.key ? '' : undefined}
                data-now={i() === 0 && !live() ? '' : undefined}
                data-at={mark.at_ms}
                onClick={() => props.onPick(mark.at_ms)}
              >
                <Cell>{thousands(mark.round)}</Cell>
                <Cell><span class={`tierchip t-${mark.tier}`}>{tierName(mark.tier)}</span></Cell>
                <Cell>{markText(mark.what)}</Cell>
              </Row>
            )}
          </For>
        </Table>
      </div>
    </section>
  );
}
