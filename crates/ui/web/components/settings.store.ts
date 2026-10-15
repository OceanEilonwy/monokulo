// What a settings form's islands share (crates/ui/src/components/settings.rs):
// the form's behaviour makes one store for its <form>; its cards, settings
// and save bar find it from their own elements. A fixi swap brings a new
// form, and so a new store; the old one goes with its form.
import { createMemo, createRoot, createSignal } from 'solid-js';
import type { Accessor } from 'solid-js';

/** A card, as the form and the bar see it. */
export interface CardHandle {
  readonly name: string;
  readonly title: string;
  readonly element: HTMLElement;
  /** Its unsaved changes (0 while hidden). */
  readonly count: Accessor<number>;
  /** Refused by the last save, and not put back since. */
  readonly refused: Accessor<boolean>;
  /** Puts its settings back as saved; false when the page reloads to. */
  readonly discard: () => boolean;
}

/** Where leaving is asked about: the link, and what it says. */
export interface Leaving {
  readonly href: string;
  readonly place: string;
}

export interface FormStore {
  readonly form: HTMLFormElement;
  /** What has unsaved changes, in the leave question ("Profile"). */
  readonly label: string;
  /** Bumped whenever a control may have changed: what's derived re-reads. */
  readonly version: Accessor<number>;
  readonly changed: () => void;
  /** Something was changed since the form came: until then, a refused
   *  save's message is what the bar says. */
  readonly touched: Accessor<boolean>;
  readonly cards: Accessor<readonly CardHandle[]>;
  readonly addCard: (card: CardHandle) => () => void;
  readonly total: Accessor<number>;
  readonly changedCards: Accessor<readonly CardHandle[]>;
  readonly leaving: Accessor<Leaving | undefined>;
  readonly setLeaving: (leaving: Leaving | undefined) => void;
  readonly saving: Accessor<boolean>;
  readonly setSaving: (saving: boolean) => void;
  /** The bar as the server rendered it: whether it says a save was
   *  refused, and how many cards were. */
  readonly loaded: { failed: boolean; refusedCards: number };
  /** Where to go once a save is answered, from "Save and go". */
  goAfterSave: string | undefined;
  /** The page is loading again (Discard on a refused card): no question. */
  reloading: boolean;
  readonly discardAll: () => void;
  readonly dispose: () => void;
}

const stores = new WeakMap<HTMLFormElement, FormStore>();

/** The store of the form `element` is in, if it's a settings form. */
export function storeOf(element: Element | null | undefined): FormStore | undefined {
  const form = element?.closest('form');
  return form ? stores.get(form) : undefined;
}

/** Makes the store for `form`: the form's behaviour does, as it attaches. */
export function createStore(form: HTMLFormElement, label: string): FormStore {
  return createRoot(dispose => {
    // Written as islands attach (their refs), not only in handlers.
    const owned = { ownedWrite: true };
    const [version, setVersion] = createSignal(0, owned);
    const [touched, setTouched] = createSignal(false, owned);
    const [cards, setCards] = createSignal<readonly CardHandle[]>([], owned);
    const [leaving, setLeaving] = createSignal<Leaving | undefined>(undefined, owned);
    const [saving, setSaving] = createSignal(false, owned);
    const total = createMemo(() => cards().reduce((sum, card) => sum + card.count(), 0));
    const changedCards = createMemo(() => cards().filter(card => card.count() > 0));
    const bar = form.querySelector('.save-bar');
    const store: FormStore = {
      form,
      label,
      version,
      changed() {
        setTouched(true);
        setVersion(v => v + 1);
      },
      touched,
      cards,
      addCard(card) {
        setCards(list => [...list, card]);
        return () => setCards(list => list.filter(other => other !== card));
      },
      total,
      changedCards,
      leaving,
      setLeaving,
      saving,
      setSaving,
      loaded: {
        failed: bar?.classList.contains('is-failed') ?? false,
        refusedCards: form.querySelectorAll('.settings-card.is-failed').length,
      },
      goAfterSave: undefined,
      reloading: false,
      discardAll() {
        for (const card of cards()) if (card.count() > 0 || card.refused()) card.discard();
        store.changed();
      },
      dispose() {
        stores.delete(form);
        dispose();
      },
    };
    stores.set(form, store);
    return store;
  });
}

// ---- values: what a control holds, and what's saved -----------------------

type Control = HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement;

const ticks = (el: Control): el is HTMLInputElement => el.type === 'checkbox' || el.type === 'radio';

function savedValue(el: Control): string | boolean {
  const saved = el.getAttribute('data-saved');
  if (saved !== null) return ticks(el) ? saved === 'on' : saved;
  if (ticks(el)) return el.defaultChecked;
  if (el instanceof HTMLSelectElement) {
    const chosen = Array.from(el.options).find(option => option.defaultSelected);
    return chosen ? chosen.value : (el.options[0]?.value ?? '');
  }
  return el.defaultValue;
}

const currentValue = (el: Control) => (ticks(el) ? el.checked : el.value);

/** What a setting is sent under: its own controls, not hidden ones. */
export function controls(setting: Element): Control[] {
  return Array.from(setting.querySelectorAll<Control>('input[name]:not([type=hidden]), select[name], textarea[name]'));
}

/** Whether a setting differs from what's saved. */
export function settingChanged(setting: Element): boolean {
  return controls(setting).some(el => currentValue(el) !== savedValue(el));
}

/** Puts a setting's controls back as saved, saying so to their scripts. */
export function restore(setting: Element): void {
  for (const el of controls(setting)) {
    const saved = savedValue(el);
    if (ticks(el)) el.checked = saved as boolean;
    else el.value = saved as string;
    el.dispatchEvent(new Event('change', { bubbles: true }));
  }
}

/** Fires one of the settings events page scripts listen for. */
export function fire(target: EventTarget, name: string, detail: unknown, cancelable = false): boolean {
  return target.dispatchEvent(new CustomEvent(name, { bubbles: true, cancelable, detail }));
}
