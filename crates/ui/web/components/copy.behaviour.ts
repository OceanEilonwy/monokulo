// The CopyValue component's behaviour (crates/ui/src/components/copy.rs):
// a Copy button for the whole value, and a shortened form on screen. The
// value's own text stays the source of truth: copied as it is at the click,
// shown whole when focused.
import { createMemo, createSignal, onCleanup, untrack } from 'solid-js';
import type { CopyValueBehaviour, CopyValueProps } from '../../solid/CopyValue';
import { truncateValue } from './truncate';
import { COPY_BY_HAND, copyText, selectContents } from './clipboard';

export function setup(props: CopyValueProps): CopyValueBehaviour {
  let value: HTMLElement | undefined;
  const [status, setStatus] = createSignal('');
  const [busy, setBusy] = createSignal(false);
  // The value's text as it is on the page, which a script or a renderer
  // may change in place: set as the behaviour attaches, so `ownedWrite`.
  // Its first value as given: the element's own text takes over once attached.
  const [text, setText] = createSignal(untrack(() => props.value), { ownedWrite: true });

  const preview = createMemo(() => {
    const full = text();
    const shown = truncateValue(full, props.truncate ?? 'end', props.segments ?? '', props.head ?? 8, props.tail ?? 8);
    return shown === full ? undefined : shown;
  });

  // The whole value focused and selected: to read, or to copy by hand.
  const selectAll = () => {
    if (value) selectContents(value);
  };

  return {
    preview,
    status,
    busy,
    reveal: selectAll,
    async copy() {
      setBusy(true);
      setStatus('');
      // Read at the click: a renderer may have changed it in place.
      const text = value?.textContent ?? props.value;
      try {
        setStatus((await copyText(text, selectAll)) ? 'Copied' : COPY_BY_HAND);
      } finally {
        setBusy(false);
      }
    },
    value(element) {
      value = element;
      const read = () => setText(element.textContent ?? '');
      const observer = new MutationObserver(read);
      observer.observe(element, { childList: true, subtree: true, characterData: true });
      onCleanup(() => observer.disconnect());
      read();
    },
  };
}
