// The chain strip: the blocks around the node's tip, the groups of stores by
// the block their scan has reached, the node's tip and the high-water mark,
// and the network's nodes. The cells and pills are drawn from the state; where
// the labels go is measured once they are drawn (an effect around the
// measuring), and the strip shows as many blocks as its width holds.
import { createEffect, createMemo, For, onSettled, Show } from 'solid-js';
import { thousands } from './format.ts';
import { cellsThatFit, stackLabels, visibleBlocks } from './layout.ts';
import type { ChainView } from './present.ts';
import { setStripWidth, stripWidth } from './state.ts';

/** One block: recorded, at the node, cached, replaced or still to come. Its
 *  passing classes (`enter`, `flash`, `probe`) are the animations'. */
function Block(props: { height: number; chain: ChainView }) {
  const chain = () => props.chain;
  const next = () => {
    const c = chain();
    return c.next_block && c.tip !== null && props.height === c.tip + 1 ? c.next_block : null;
  };
  const state = () => {
    const c = chain();
    if (c.tip !== null && props.height > c.tip) return 'ghost';
    if (c.replaced.includes(props.height)) return 'reorg';
    if (c.cached.includes(props.height)) return 'cached';
    if (c.high_water !== null && props.height > c.high_water) return 'new';
    return '';
  };
  const saved = () => chain().checkpoints.find(([height]) => height === props.height)?.[1];
  const fill = () => {
    const scanning = chain().scanning;
    return scanning && scanning[0] === props.height ? scanning[1] : saved() ?? 0;
  };
  return (
    <div
      data-h={props.height}
      class={{ cell: true, ghost: state() === 'ghost', reorg: state() === 'reorg', cached: state() === 'cached', new: state() === 'new', next: !!next(), over: !!next()?.over }}
      title={next()?.title ?? `Block ${thousands(props.height)}`}
    >
      <span class="fill" style={{ width: `${(fill() * 100).toFixed(0)}%` }} />
      <Show when={saved() !== undefined}><span class="save" /></Show>
      <Show when={next()}>
        {(block) => (
          <>
            <span class="pool" style={{ height: `${((block().fill ?? 0) * 100).toFixed(0)}%` }} />
            <span class="cnt">{block().count}</span>
          </>
        )}
      </Show>
    </div>
  );
}

export function Chain(props: { chain: ChainView }) {
  let scroller!: HTMLDivElement, strip!: HTMLDivElement, cells!: HTMLDivElement;
  let marks!: HTMLDivElement, axis!: HTMLDivElement, pills!: HTMLDivElement;
  const blocks = createMemo(() => visibleBlocks(props.chain, cellsThatFit(stripWidth() || 800)));
  // Newest first: the strip lays them out from the right.
  const newestFirst = createMemo(() => [...blocks()].reverse());

  // The strip's width sets how many blocks it shows.
  onSettled(() => {
    const observer = new ResizeObserver(() => setStripWidth(strip.clientWidth));
    observer.observe(scroller);
    return () => observer.disconnect();
  });

  // Where the labels go, once the cells are drawn: each block's centre is
  // measured, and the labels stacked as few levels high as they can go,
  // each with its left and right edges where it is going (a pill glides
  // there).
  createEffect(() => [props.chain, blocks()] as const, ([chain, shown]) => {
    const cellOf = (height: number) => cells.querySelector<HTMLElement>(`[data-h="${height}"]`);
    const centre = (element: HTMLElement) => element.offsetLeft + element.offsetWidth / 2;
    const first = shown.find((height): height is number => height !== null);
    const oldest = first === undefined ? null : cellOf(first);
    const x = (height: number) => {
      const cell = cellOf(height);
      if (cell) return centre(cell);
      // Older than every block drawn: at the oldest, the left edge.
      if (first !== undefined && height < first && oldest) return centre(oldest);
      // In the cut.
      const cut = cells.querySelector<HTMLElement>('.brk');
      if (cut) return centre(cut);
      return oldest ? centre(oldest) : 0;
    };
    const label = (cls: string, text: string, left: number) => {
      const span = document.createElement('span');
      if (cls) span.className = cls;
      span.textContent = text;
      span.style.left = `${left}px`;
      return span;
    };
    const tip = chain.tip ?? 0, high = chain.high_water ?? 0;
    const tipLabel = chain.tip !== null ? label('m-tip', 'node tip', x(tip)) : null;
    const highLabel = high < tip ? label('m-hw', 'scanned to', x(high)) : null;
    let window: HTMLElement | null = null;
    if (chain.window_from !== null) {
      const from = Math.max(chain.window_from, first ?? chain.window_from);
      window = label('m-win', '', x(from) - 11);
      window.title = 'The last blocks checked again for a reorganisation every round';
      window.style.width = `${x(high) - x(from) + 22}px`;
    }
    marks.replaceChildren(...[tipLabel, highLabel, window].filter((element): element is HTMLElement => element !== null));
    axis.replaceChildren(...shown
      .filter((height): height is number => height !== null && height % 5 === 0 && height <= tip)
      .map((height) => label('', thousands(height), x(height))));
    const placed: { element: HTMLElement; span: [number, number] }[] = [];
    const anchored = (element: HTMLElement, at: number, fromLeft: boolean, off: number) => {
      const width = element.offsetWidth;
      placed.push({ element, span: fromLeft ? [at - off, at - off + width] : [at + off - width, at + off] });
    };
    if (tipLabel) anchored(tipLabel, x(tip), true, 10);
    if (highLabel) anchored(highLabel, x(high), false, 10);
    for (const group of chain.groups) {
      const pill = pills.querySelector<HTMLElement>(`[data-id="${group.id}"]`);
      if (!pill) continue;
      pill.style.left = `${x(group.cursor)}px`;
      anchored(pill, x(group.cursor), !group.frontier, 12);
    }
    const [levels, used] = stackLabels(placed.map((label) => label.span));
    placed.forEach((label, i) => label.element.style.setProperty('--level', String(levels[i])));
    strip.style.setProperty('--levels', String(used));
  });

  return (
    <section class="engine-card t-blocks" id="engine-chain" aria-labelledby="h-chain">
      <header>
        <h2 id="h-chain">Chain</h2>
        <div class="legend">
          <span><i class="cell" aria-hidden="true" />recorded</span>
          <span><i class="cell new" aria-hidden="true" />at the node</span>
          <span><i class="cell cached" aria-hidden="true" />in the cache</span>
          <span><i class="cell reorg" aria-hidden="true" />replaced</span>
          <span><i class="cell ghost next" aria-hidden="true"><span class="pool" /></i>next block: the node's pool</span>
          <span><i class="saved-glyph" aria-hidden="true" />saved to disk</span>
        </div>
        <span class="engine-chip engine-right" id="cache-chip">{props.chain.cache}</span>
      </header>
      <div class="chain-row">
        <div class="strip-scroll" id="strip-scroll" ref={scroller}>
          <div class="strip" id="strip" ref={strip}>
            <div class="marks" id="chain-marks" ref={marks} />
            <div class="cells" id="cells" ref={cells}>
              <For each={newestFirst()}>
                {(height) => (height === null ? <div class="brk">…</div> : <Block height={height} chain={props.chain} />)}
              </For>
            </div>
            <div class="axis" id="chain-axis" ref={axis} />
            <div class="pills" id="pills" ref={pills}>
              <For each={props.chain.groups} keyed={(group) => group.id}>
                {(group) => (
                  <div
                    class={{ pill: true, frontier: group().frontier, catchup: !group().frontier, busy: group().busy, waiting: group().waiting }}
                    data-id={group().id}
                    title={group().title}
                  >
                    {group().label}
                  </div>
                )}
              </For>
            </div>
          </div>
        </div>
        <div class="nodes" id="nodes">
          {/* The network's nodes, and nothing else: the last call to the node
              in use under its name. */}
          <For each={props.chain.nodes} keyed={(node) => node.label}>
            {(node) => (
              <div class="node">
                <div class="nm"><span class="label" title={node().label}>{node().label}</span><span class={`engine-chip ${node().tone}`}>{node().chip}</span></div>
                <Show when={node().active && props.chain.call}>{(call) => <div class="call">{call()}</div>}</Show>
              </div>
            )}
          </For>
        </div>
      </div>
    </section>
  );
}
