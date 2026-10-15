// The save bar's behaviour (crates/ui/src/components/settings.rs): shown
// only while something changed, a save was refused, or leaving is asked
// about; naming the cards changed; the leave question's choices; Save busy
// while a save is out.
import { createMemo, createSignal, flush } from 'solid-js';
import type { SaveBarBehaviour, SaveBarProps } from '../../solid/SaveBar';
import type { ChangedCard } from '../../solid/types';
import { storeOf, type FormStore } from './settings.store';

export function setup(props: SaveBarProps): SaveBarBehaviour {
  const [store, setStore] = createSignal<FormStore | undefined>(undefined, { ownedWrite: true });
  let save: HTMLElement | undefined;
  let goSave: HTMLElement | undefined;

  const total = () => store()?.total() ?? 0;
  const leaving = () => store()?.leaving();
  const failedAtLoad = () => store()?.loaded.failed ?? (props.failed ?? false);
  // The bar's own message: once something is changed (or there was no
  // refusal to keep saying).
  const own = createMemo(() => !leaving() && total() > 0 && ((store()?.touched() ?? false) || !failedAtLoad()));
  // A refusal on cards lasts while one of them still shows it; one that
  // names no card, until the next save.
  const failed = createMemo(() => {
    const form = store();
    if (!form || own() || leaving() || !failedAtLoad()) return false;
    return form.loaded.refusedCards === 0 || form.cards().some(card => card.refused());
  });

  const go = (href: string) => {
    const link = document.querySelector<HTMLAnchorElement & { __go?: boolean }>(`[data-leave-asks] a[href="${CSS.escape(href)}"]`);
    // Marked only for this click, so a later click on it asks again.
    if (link) {
      link.__go = true;
      link.click();
      link.__go = false;
    } else {
      location.href = href;
    }
  };

  return {
    idle: () => !(total() > 0 || failed() || !!leaving()),
    dirty: () => total() > 0,
    failed,
    leaving: () => !!leaving(),
    own: () => own() || !!leaving(),
    count: () => (own() ? `${total()} unsaved change${total() === 1 ? '' : 's'}` : undefined),
    cards: () => {
      if (!own()) return [];
      const list = store()?.changedCards() ?? [];
      return list.map((card, i): ChangedCard => ({
        name: card.name,
        title: card.title,
        before: i === 0 ? '' : i === list.length - 1 ? ' and ' : ', ',
      }));
    },
    leaving_title: () => (leaving() ? `${store()?.label || 'This page'} has unsaved changes.` : undefined),
    leaving_text: () => {
      const place = leaving()?.place;
      return place === undefined ? undefined : `Save or discard them before going to ${place}.`;
    },
    saving: () => store()?.saving() ?? false,
    discard_all(event) {
      const form = store();
      if (!form) return;
      event.preventDefault();
      form.discardAll();
    },
    stay() {
      store()?.setLeaving(undefined);
    },
    discard_and_go() {
      const form = store();
      const href = form?.leaving()?.href;
      if (!form || href === undefined) return;
      form.setLeaving(undefined);
      form.discardAll();
      if (!form.reloading) go(href);
    },
    save_and_go() {
      const form = store();
      const href = form?.leaving()?.href;
      if (!form || href === undefined) return;
      form.goAfterSave = href;
      form.setLeaving(undefined);
      flush();
      form.form.requestSubmit((save as HTMLButtonElement | undefined) ?? undefined);
    },
    root(element) {
      setStore(storeOf(element));
    },
    save(element) {
      save = element;
    },
    go_save(element) {
      goSave = element;
      // The question's choice to take, once it's asked.
      queueMicrotask(() => goSave?.focus());
    },
  };
}
