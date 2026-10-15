// The status dot's behaviour (crates/ui/src/components/chrome.rs): asks
// how the instance is every 30 seconds while the tab is in sight, at once
// when the page didn't know.
import { createSignal, onCleanup } from 'solid-js';
import type { StatusDotBehaviour, StatusDotProps } from '../../solid/StatusDot';
import type { HealthState } from '../../solid/types';

const POLL_MS = 30_000;
const WORDS: Record<HealthState, string> = {
  ok: 'all systems healthy',
  slow: 'scanning is slow - see the status page',
  error: 'an issue was detected - see the status page',
  unknown: 'could not check status',
};

export function setup(props: StatusDotProps): StatusDotBehaviour {
  const [now, setNow] = createSignal<HealthState>(props.health);
  const [words, setWords] = createSignal(props.health === 'unknown' ? 'status not checked yet' : WORDS[props.health]);
  const show = (state: HealthState) => {
    setNow(state);
    setWords(WORDS[state]);
  };
  if (props.poll) {
    let timer: ReturnType<typeof setTimeout>;
    const poll = () => {
      if (document.hidden) {
        timer = setTimeout(poll, POLL_MS);
        return;
      }
      fetch(props.poll!, { cache: 'no-store' })
        .then(response => {
          if (!response.ok) throw new Error('status unavailable');
          return response.json();
        })
        .then((data: { state?: string }) => show(data?.state === 'ok' ? 'ok' : data?.state === 'slow' ? 'slow' : 'error'))
        .catch(() => show('unknown'))
        .finally(() => { timer = setTimeout(poll, POLL_MS); });
    };
    timer = setTimeout(poll, props.health === 'unknown' ? 0 : POLL_MS);
    onCleanup(() => clearTimeout(timer));
  }
  return { now, words };
}
