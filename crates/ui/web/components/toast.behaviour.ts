// A toast's behaviour (crates/ui/src/components/settings.rs): a close
// button. Without JavaScript every toast fades by itself; with it, one
// saying something wasn't saved stays until it's closed (settings.css).
import { createSignal } from 'solid-js';
import type { ToastBehaviour, ToastProps } from '../../solid/Toast';

export function setup(_props: ToastProps): ToastBehaviour {
  const [closing, setClosing] = createSignal(false);
  let root: HTMLElement | undefined;
  return {
    closing,
    close() {
      setClosing(true);
      // Gone once it has faded: its island lets go as it leaves the page.
      setTimeout(() => root?.remove(), 260);
    },
    root(element) {
      root = element;
    },
  };
}
