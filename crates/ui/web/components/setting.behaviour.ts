// A setting's behaviour (crates/ui/src/components/settings.rs): "changed"
// while it differs from what's saved.
import { createMemo, createSignal } from 'solid-js';
import type { SettingBehaviour, SettingProps } from '../../solid/Setting';
import { settingChanged, storeOf, type FormStore } from './settings.store';

export function setup(_props: SettingProps): SettingBehaviour {
  const [root, setRoot] = createSignal<HTMLElement | undefined>(undefined, { ownedWrite: true });
  const [store, setStore] = createSignal<FormStore | undefined>(undefined, { ownedWrite: true });
  const changed = createMemo(() => {
    const element = root();
    const form = store();
    if (!element || !form) return false;
    form.version();
    return settingChanged(element);
  });
  return {
    changed,
    root(element) {
      setRoot(element);
      setStore(storeOf(element));
    },
  };
}
