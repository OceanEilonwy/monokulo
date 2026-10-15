// A help tip's behaviour (crates/ui/src/components/content.rs): its
// `details` opens and closes by itself; this closes it on a click outside
// it, and on Escape, putting focus back on its "?".
import { onCleanup } from 'solid-js';
import type { HelpTipBehaviour, HelpTipProps } from '../../solid/HelpTip';

export function setup(_props: HelpTipProps): HelpTipBehaviour {
  return {
    root(element) {
      const tip = element as HTMLDetailsElement;
      const outside = (event: Event) => {
        if (tip.open && !tip.contains(event.target as Node)) tip.open = false;
      };
      const escape = (event: KeyboardEvent) => {
        if (event.key !== 'Escape' || !tip.open) return;
        tip.open = false;
        tip.querySelector('summary')?.focus();
        event.preventDefault();
      };
      document.addEventListener('click', outside);
      document.addEventListener('keydown', escape);
      onCleanup(() => {
        document.removeEventListener('click', outside);
        document.removeEventListener('keydown', escape);
      });
    },
  };
}
