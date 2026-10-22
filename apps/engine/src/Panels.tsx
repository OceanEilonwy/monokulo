// The panels under the round, each a line until opened: Scanning first and
// open; Reorg opening by itself while the node's chain differs from the
// recorded one, and closing again if it opened by itself.
import { createEffect, For, Show } from 'solid-js';
import { Alert, Disclosure } from '@monokulo/ui/solid';
import type { ScanningView } from './machine.ts';
import type { Panel, Side } from './present.ts';

function Rows(props: { panel: Panel }) {
  return (
    <div class="kv">
      <For each={props.panel.rows} keyed={false}>{(row) => <><span>{row()[0]}</span><b>{row()[1]}</b></>}</For>
    </div>
  );
}

function Body(props: { panel: Panel; explain: string }) {
  return (
    <>
      <p class="explain">{props.explain}</p>
      <Rows panel={props.panel} />
    </>
  );
}

function Scanning(props: { scanning: ScanningView | null }) {
  return (
    <Show when={props.scanning} fallback={<p class="explain">The engine hasn't reported how its scan of this network is going.</p>}>
      {(scanning) => (
        <>
          <Show when={scanning().slow}>{(slow) => <Alert tone="notice" data-slow-block="">{slow()}</Alert>}</Show>
          <dl class="scan-figures">
            <For each={scanning().rows} keyed={false}>{(row) => <><dt>{row()[0]}</dt><dd>{row()[1]}</dd></>}</For>
          </dl>
        </>
      )}
    </Show>
  );
}

export function Panels(props: { side: Side; scanning: ScanningView | null }) {
  let reorg!: HTMLDetailsElement;
  // Opened by itself while the alert is up, and closed again when it goes,
  // unless it was opened by hand.
  let opened = false;
  createEffect(() => props.side.reorg.alert, (alert) => {
    if (alert && !opened && !reorg.open) [reorg.open, opened] = [true, true];
    else if (!alert && opened) [reorg.open, opened] = [false, false];
  });
  return (
    <section class="engine-panels" id="engine-panels" aria-label="More about this network">
      <Disclosure
        look="compact"
        id="d-scanning"
        open={true}
        summary={<><span class="tierchip t-blocks">Scanning</span><span class="sum" id="scan-sum"><span>{props.scanning?.preview ?? 'not reported'}</span></span></>}
      >
        <div id="scan-body"><Scanning scanning={props.scanning} /></div>
      </Disclosure>
      <Disclosure
        look="compact"
        id="d-reorg"
        ref={reorg}
        alert={props.side.reorg.alert}
        summary={<><span class="tierchip t-chain">Reorg</span><span class="sum"><span>{props.side.reorg.summary}</span></span></>}
      >
        <Body panel={props.side.reorg} explain="Opens by itself while the node's chain differs from the recorded one. Blocks wait, and no order is settled as paid, until the rewind." />
      </Disclosure>
      <Disclosure
        look="compact"
        id="d-mempool"
        summary={
          <>
            <span class="tierchip t-mempool">Mempool</span>
            <span class="sum">
              <span class="beat" id="beat" title="the fast path's last pass" />
              <span class="dots" id="pool-dots">
                <For each={props.side.pool_txs} keyed={false}>
                  {(tx) => <span class={{ dot: true, match: tx()[1] }} title={`Transaction ${tx()[0]}`} />}
                </For>
              </span>
              <span>{props.side.mempool.summary}</span>
            </span>
          </>
        }
      >
        <Body panel={props.side.mempool} explain="The node's pool is what the next block is mined from; the page asks the node about it every 5s while it is open. The engine itself looks at the pool only while an order could be paid from it: the fast path every 250ms for transactions it hasn't seen, the round's rotation rescanning the rest as a safety net." />
      </Disclosure>
      <Disclosure
        look="compact"
        id="d-orders"
        summary={
          <>
            <span class="tierchip t-settlement">Order status</span>
            <span class="sum" id="orders-sum">
              <span>{props.side.orders.summary}</span>
              <Show when={props.side.transitions[0]}>
                {(last) => <><span class="engine-hint">last</span><span class={`engine-state state-${last()[1]}`}>{last()[1]}</span></>}
              </Show>
            </span>
          </>
        }
      >
        <p class="explain">The settlement tier: it turns what the chain and the pool say into each order's status, and writes the order event Monokulo sends the shop's webhooks from. An order is recomputed when one of its payments changed, when it expires, and on each new block while it is confirming.</p>
        <Rows panel={props.side.orders} />
        <div id="orders-transitions">
          <For each={props.side.transitions} keyed={false}>
            {(t) => <div><span class={`engine-state state-${t()[0]}`}>{t()[0]}</span> to <span class={`engine-state state-${t()[1]}`}>{t()[1]}</span></div>}
          </For>
        </div>
      </Disclosure>
      <Disclosure
        look="compact"
        id="d-upkeep"
        summary={<><span class="tierchip t-upkeep">Upkeep</span><span class="sum"><span class="upd" id="upd"><i /><i /><i /><i /></span><span>{props.side.upkeep.summary}</span></span></>}
      >
        <Body panel={props.side.upkeep} explain="Pruning old block hashes, the database checkpoint, rechecking voided payments and orders' scanned ranges: a little every round." />
      </Disclosure>
      <Disclosure
        look="compact"
        id="d-database"
        summary={
          <>
            <span class="tierchip t-other">Database</span>
            <span class="sum" id="db-sum">
              <span class="minibars" title="Scanner and Admin queues">
                <For each={props.side.queues} keyed={false}>{(queued) => <i style={{ height: `${2 + Math.min(2, queued()) * 6}px` }} />}</For>
              </span>
              <span>{props.side.database.summary}</span>
            </span>
          </>
        }
      >
        <Body panel={props.side.database} explain="One thread runs every database job; each queue gets a turn in rotation, so no kind of work waits behind another for more than one job." />
      </Disclosure>
      <Disclosure
        look="compact"
        id="d-restart"
        summary={<><span class="tierchip t-other">Restart safety</span><span class="sum"><span class="saved-glyph" aria-hidden="true" /><span>{props.side.restart.summary}</span></span></>}
      >
        <Body panel={props.side.restart} explain="A dark square marks each save on the thing saved. A restart loses only what is in memory, at the cost of some repeated work." />
      </Disclosure>
    </section>
  );
}
