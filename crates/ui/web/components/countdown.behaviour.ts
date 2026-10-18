// The Countdown component's behaviour (crates/ui/src/components/steps.rs):
// counts down from the seconds the server said were left, by the browser's
// clock from the moment it attaches (so a wrong clock can't skew it), each
// second as m:ss; the bar steps each minute. Screen readers hear it only as
// it crosses `soon`, `urgent` and the end.
import { createMemo, createSignal, onCleanup, untrack } from 'solid-js';
import type { CountdownBehaviour, CountdownProps } from '../../solid/Countdown';

type Tone = 'normal' | 'soon' | 'urgent';

export function setup(props: CountdownProps): CountdownBehaviour {
  const soon = () => props.soon ?? 300;
  const urgent = () => props.urgent ?? 60;
  const endsAt = Date.now() + untrack(() => props.remaining) * 1000;
  const secondsLeft = () => Math.max(0, Math.ceil((endsAt - Date.now()) / 1000));
  const [left, setLeft] = createSignal(secondsLeft());
  const [status, setStatus] = createSignal('');

  const tone = createMemo<Tone>(() => (left() <= urgent() ? 'urgent' : left() <= soon() ? 'soon' : 'normal'));

  let root: Element | null = null;
  let announced: Tone | 'over' = untrack(tone);
  const timer = setInterval(() => {
    const now = secondsLeft();
    setLeft(now);
    const crossed = now === 0 ? 'over' : tone();
    if (crossed !== announced) {
      announced = crossed;
      if (crossed === 'over') {
        setStatus('Time is up.');
        clearInterval(timer);
        root?.dispatchEvent(new CustomEvent('mk-countdown-end', { bubbles: true }));
      } else if (crossed !== 'normal') {
        const minutes = Math.ceil(now / 60);
        setStatus(`${minutes} minute${minutes === 1 ? '' : 's'} left.`);
      }
    }
  }, 1000);
  onCleanup(() => clearInterval(timer));

  return {
    clock: () => {
      const s = left();
      return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
    },
    bar: () => Math.min(props.total, Math.ceil(left() / 60) * 60),
    now: tone,
    status,
    root(element) {
      root = element;
    },
  };
}
