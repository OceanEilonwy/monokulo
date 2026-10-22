// The summary row: the node's tip, how far the scan has got, the stores,
// the last round and whether the chain agrees with the node.
import { For } from 'solid-js';
import type { Figure } from './present.ts';

export function Summary(props: { figures: Figure[] }) {
  return (
    <section class="engine-card engine-summary" id="engine-summary" aria-label="Summary">
      {/* By place: the same six figures, their words changing. */}
      <For each={props.figures} keyed={false}>
        {(figure) => (
          <div><span class="k">{figure().label}</span><span class="v">{figure().value}</span><span class="s">{figure().note}</span></div>
        )}
      </For>
    </section>
  );
}
