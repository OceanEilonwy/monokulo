// The Tabs component's behaviour (crates/ui/src/components/tabs.rs): one
// panel at a time, picked with a click, the arrow keys, Home or End. The
// tab the page's address names is the first one shown.
import { createSignal } from 'solid-js';
import type { TabsBehaviour, TabsProps } from '../../solid/Tabs';

export function setup(props: TabsProps): TabsBehaviour {
  const ids = () => props.tabs.map(tab => tab.id);
  const linked = typeof location === 'undefined' ? '' : decodeURIComponent(location.hash.slice(1));
  const [selected, setSelected] = createSignal(ids().includes(linked) ? linked : (ids()[0] ?? ''));

  const show = (id: string, focus: boolean) => {
    setSelected(id);
    if (focus) document.getElementById(`${id}-tab`)?.focus();
  };

  return {
    selected,
    pick(id, event) {
      event.preventDefault();
      show(id, false);
    },
    key(event) {
      const e = event as KeyboardEvent;
      const list = ids();
      const at = list.indexOf(selected());
      const next = {
        ArrowRight: (at + 1) % list.length,
        ArrowLeft: (at + list.length - 1) % list.length,
        Home: 0,
        End: list.length - 1,
      }[e.key];
      if (next === undefined) return;
      e.preventDefault();
      show(list[next], true);
    },
  };
}
