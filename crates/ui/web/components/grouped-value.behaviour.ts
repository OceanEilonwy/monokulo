// The GroupedValue component's behaviour (crates/ui/src/components/copy.rs):
// a Copy button for the whole value. It says "Copied" for a few seconds,
// then goes back; the value itself never changes or collapses.
import { createSignal, onCleanup } from 'solid-js';
import type { GroupedValueBehaviour, GroupedValueProps } from '../../solid/GroupedValue';
import { COPY_BY_HAND, copyText } from './clipboard';

const SHOW_COPIED_MS = 4000;

export function setup(props: GroupedValueProps): GroupedValueBehaviour {
  let shown: HTMLElement | undefined;
  const [copied, setCopied] = createSignal(false);
  const [status, setStatus] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  let reset: ReturnType<typeof setTimeout> | undefined;
  onCleanup(() => clearTimeout(reset));

  // The groups selected, for a copy by hand: they hold the value with no
  // spaces between them, so the selection is the value itself.
  const select = () => {
    if (!shown) return;
    const range = document.createRange();
    range.selectNodeContents(shown);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
  };

  return {
    copied,
    status,
    busy,
    async copy() {
      setBusy(true);
      setStatus('');
      clearTimeout(reset);
      try {
        const done = await copyText(props.value, select);
        setCopied(done);
        setStatus(done ? 'Copied' : COPY_BY_HAND);
        if (done) reset = setTimeout(() => setCopied(false), SHOW_COPIED_MS);
      } finally {
        setBusy(false);
      }
    },
    shown(element) {
      shown = element;
    },
  };
}
