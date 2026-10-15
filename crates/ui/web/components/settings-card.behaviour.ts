// A settings card's behaviour (crates/ui/src/components/settings.rs): its
// unsaved changes counted and Discard for them; a refusal put back once
// its settings are; shown only while its `shown_by` checkbox is ticked.
import { createEffect, createMemo, createSignal, onCleanup } from 'solid-js';
import type { SettingsCardBehaviour, SettingsCardProps } from '../../solid/SettingsCard';
import { fire, restore, settingChanged, storeOf, type FormStore } from './settings.store';

export function setup(props: SettingsCardProps): SettingsCardBehaviour {
  const owned = { ownedWrite: true };
  const [root, setRoot] = createSignal<HTMLElement | undefined>(undefined, owned);
  const [store, setStore] = createSignal<FormStore | undefined>(undefined, owned);
  // The refusal the page came with, put back (a discard, or its settings
  // all as saved again).
  const [cleared, setCleared] = createSignal(false, owned);

  // The checkbox `shown_by` names, ticked or not; shown when there is none.
  const shown = createMemo(() => {
    const element = root();
    const form = store();
    if (!props.shown_by || !element) return true;
    form?.version();
    const at = props.shown_by.indexOf('=');
    const name = props.shown_by.slice(0, at), value = props.shown_by.slice(at + 1);
    const box = Array.from(document.querySelectorAll<HTMLInputElement>('input[type=checkbox]'))
      .find(el => el.name === name && el.value === value);
    return box ? box.checked : !(props.hidden ?? false);
  });

  const count = createMemo(() => {
    const element = root();
    const form = store();
    if (!element || !form || !shown()) return 0;
    form.version();
    const own = Array.from(element.querySelectorAll('.setting-field')).filter(settingChanged).length;
    // A page script adds what it tracks itself (the admin page's node rows).
    const extra = { changes: 0 };
    element.dispatchEvent(new CustomEvent('mk-count', { detail: extra }));
    return own + extra.changes;
  });

  const refused = () => (props.failed ?? false) && !cleared();

  // A refused card whose settings are all as saved again has nothing left
  // to refuse.
  createEffect(
    () => (store()?.touched() ?? false) && count() === 0 && (props.failed ?? false),
    put => {
      if (put) setCleared(true);
    },
  );

  // Hidden, its controls are off too, so what's in them isn't sent unseen.
  createEffect(() => ({ visible: shown(), element: root() }), ({ visible, element }) => {
    if (!element) return;
    for (const el of Array.from(element.querySelectorAll<HTMLInputElement>('input, select, textarea'))) {
      if (!visible && !el.disabled) {
        el.disabled = true;
        el.setAttribute('data-hidden-off', '');
      } else if (visible && el.hasAttribute('data-hidden-off')) {
        el.disabled = false;
        el.removeAttribute('data-hidden-off');
      }
    }
  });

  const discard = (): boolean => {
    const element = root();
    const form = store();
    if (!element) return true;
    if (!fire(element, 'mk-discard', null, true)) {
      // The page puts it back by loading again.
      if (form) form.reloading = true;
      return false;
    }
    for (const setting of Array.from(element.querySelectorAll('.setting-field'))) restore(setting);
    setCleared(true);
    return true;
  };

  return {
    dirty: () => count() > 0,
    refused,
    cleared,
    unsaved: () => (count() > 0 ? `${count()} unsaved` : undefined),
    settled: () => count() === 0 && !refused(),
    off: () => !shown(),
    discard() {
      discard();
      store()?.changed();
    },
    root(element) {
      setRoot(element);
      const form = storeOf(element);
      setStore(form);
      if (!form) return;
      const title = element.querySelector('.card-head h3')?.textContent ?? props.title;
      onCleanup(form.addCard({ name: props.name, title, element, count, refused, discard }));
    },
  };
}
