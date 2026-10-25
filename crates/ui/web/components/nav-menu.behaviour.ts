// A site bar's menu of links (crates/ui/src/components/chrome.rs): its
// `details` opens and closes by itself; this closes it as a `Menu`'s
// behaviour closes that one.
import type { NavMenuBehaviour, NavMenuProps } from '../../solid/NavMenu';
import { closeWhenLeft } from './menu.behaviour';

export function setup(_props: NavMenuProps): NavMenuBehaviour {
  return {
    root(element) {
      closeWhenLeft(element as HTMLDetailsElement);
    },
  };
}
