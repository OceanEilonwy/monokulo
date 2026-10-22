// The engine page's live view: the timeline, then the summary, the
// "Machine and links" strip, the chain, the round, the panels and the
// events, top down; the legend's (?) in the page's header.
import { Show } from 'solid-js';
import { Portal } from '@solidjs/web';
import { Chain } from './Chain.tsx';
import { Events } from './Events.tsx';
import { Help } from './Help.tsx';
import { MachineStrip } from './MachineStrip.tsx';
import { Panels } from './Panels.tsx';
import { RoundCard } from './Round.tsx';
import { catchingUp, machine, machineAge, network, pinned, view } from './state.ts';
import { Summary } from './Summary.tsx';
import { Timeline } from './Timeline.tsx';

/** What the page does when someone picks something, the controller's. */
export interface Actions {
  /** Goes to a moment: an event in the table. */
  pick: (atMs: number) => void;
  /** Shows a past round in the round card. */
  pin: (number: number) => void;
  /** Back to the live round. */
  resume: () => void;
}

export function App(props: { actions: Actions; helpSlot: Element | null }) {
  return (
    <div id="engine-stage" data-network={network()} class={{ 'catching-up': catchingUp() }}>
      <Show when={props.helpSlot}>{(slot) => <Portal mount={slot()}><Help /></Portal>}</Show>
      <Timeline />
      <p class="sync-motion-note engine-hint" role="status">Catching up: showing progress with reduced animation.</p>
      <Show when={view()} fallback={<p class="engine-hint" role="status">Connecting to the engine…</p>}>
        {(shown) => (
          <>
            <Summary figures={shown().summary} />
            <Show when={machine()}>{(strip) => <MachineStrip machine={strip()} age={machineAge()} />}</Show>
            <Chain chain={shown().chain} />
            <RoundCard
              round={pinned()?.round ?? shown().round}
              ribbon={pinned()?.ribbon ?? shown().ribbon}
              pinned={pinned()?.round.number ?? null}
              onPin={props.actions.pin}
              onResume={props.actions.resume}
            />
            <Panels side={shown().side} scanning={machine()?.scanning ?? null} />
          </>
        )}
      </Show>
      <Events onPick={props.actions.pick} />
    </div>
  );
}
