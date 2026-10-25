// A menu's behaviour (crates/ui/src/components/chrome.rs): its `details`
// opens and closes by itself; this closes it on a click outside it, and on
// Escape, putting focus back on its button.
import { onCleanup } from 'solid-js';
import type { MenuBehaviour, MenuProps } from '../../solid/Menu';

export function setup(_props: MenuProps): MenuBehaviour {
  return {
    root(element) {
      closeWhenLeft(element as HTMLDetailsElement);
    },
  };
}

// Closes `menu` on a click outside it and on Escape (focus back on its
// button), until its island lets go. The site bar's `NavMenu` shares it.
export function closeWhenLeft(menu: HTMLDetailsElement) {
  const outside = (event: Event) => {
    if (menu.open && !menu.contains(event.target as Node)) menu.open = false;
  };
  const escape = (event: KeyboardEvent) => {
    if (event.key !== 'Escape' || !menu.open) return;
    menu.open = false;
    menu.querySelector('summary')?.focus();
    event.preventDefault();
  };
  document.addEventListener('click', outside);
  document.addEventListener('keydown', escape);
  onCleanup(() => {
    document.removeEventListener('click', outside);
    document.removeEventListener('keydown', escape);
  });
}
