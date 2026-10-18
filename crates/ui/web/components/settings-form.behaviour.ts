// A settings form's behaviour (crates/ui/src/components/settings.rs): the
// store its cards, settings and save bar share; following every change;
// asking before leaving with changes; Save busy while a save is out, and
// what the page says about the save it answers.
//
// Saving is the form's own: a plain POST, or fixi's swap where the form
// has `fx-action`. A swap brings a new form, which attaches again.
import { createEffect, flush, onCleanup } from 'solid-js';
import type { SettingsFormBehaviour, SettingsFormProps } from '../../solid/SettingsForm';
import { createStore, fire, type FormStore } from './settings.store';

function showCard(name: string): boolean {
  const card = document.getElementById(`card-${name}`);
  if (!card) return false;
  const reduce = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches;
  card.scrollIntoView({ block: 'start', behavior: reduce ? 'auto' : 'smooth' });
  const problem = card.querySelector('[aria-invalid="true"], .setting-problem');
  const row = problem?.closest('details');
  if (row) row.open = true;
  const scope = problem?.closest('.setting-field') ?? card;
  scope.querySelector<HTMLElement>('input:not([disabled]):not([type=hidden]), select, textarea')?.focus({ preventScroll: true });
  return true;
}

// A save that reloads the page (a plain POST, without fixi) brings a new
// page, which the browser shows from the top. The card on screen when Save
// was pressed, and how far down the window it was, are kept for the page
// the save answers with, which scrolls that card back to the same place.
const PLACE = 'mk-settings-place';

interface Place { path: string; card?: string; top: number; y: number; at: number }

function rememberPlace(form: HTMLFormElement) {
  const card = Array.from(form.querySelectorAll<HTMLElement>('.settings-card[id]'))
    .find(card => card.getBoundingClientRect().bottom > 0);
  const place: Place = { path: location.pathname, card: card?.id, top: card?.getBoundingClientRect().top ?? 0, y: window.scrollY, at: Date.now() };
  try {
    sessionStorage.setItem(PLACE, JSON.stringify(place));
  } catch {}
}

/** The place a save on this page left, if it's this save's. */
function placeLeft(): Place | undefined {
  try {
    const place = JSON.parse(sessionStorage.getItem(PLACE) ?? 'null') as Place | null;
    if (place && place.path === location.pathname && Date.now() - place.at < 60_000) return place;
  } catch {}
  return undefined;
}

function returnTo(place: Place) {
  const card = place.card ? document.getElementById(place.card) : null;
  if (card) window.scrollBy(0, card.getBoundingClientRect().top - place.top);
  else window.scrollTo(0, place.y);
}

/** What the page says about the save it answers, once its cards are in. */
function settle(host: HTMLElement, store: FormStore) {
  const bar = store.form.querySelector('.save-bar');
  const message = bar?.querySelector('[data-save-bar-message]');
  const refused = Array.from(store.form.querySelectorAll<HTMLElement>('.settings-card.is-failed'));
  for (const card of refused) {
    const why = card.querySelector('.card-body > p.error[role=alert]');
    fire(host, 'mk-failed', { group: card.dataset.name, message: why?.textContent ?? '' });
  }
  if (!refused.length && store.loaded.failed) fire(host, 'mk-failed', { group: null, message: message?.textContent ?? '' });
  const saved = Array.from(store.form.querySelectorAll('.settings-card [data-card-saved]'))
    .map(mark => mark.closest<HTMLElement>('.settings-card')?.dataset.name);
  if (saved.length) fire(host, 'mk-saved', { groups: saved });
}

export function setup(props: SettingsFormProps): SettingsFormBehaviour {
  let host: HTMLElement | undefined;

  return {
    host(element) {
      host = element;
    },
    form(element) {
      const form = element as HTMLFormElement;
      const store = createStore(form, props.label);
      onCleanup(() => store.dispose());
      // Back where a save that reloaded the page left off: now, and again
      // once the cards are in.
      const place = placeLeft();
      if (place) returnTo(place);
      const listen = <K extends string>(target: EventTarget, name: K, handler: (event: Event) => void, capture = false) => {
        target.addEventListener(name, handler, capture);
        onCleanup(() => target.removeEventListener(name, handler, capture));
      };

      // The changes, as they come; and one a page script makes itself.
      const changed = (event: Event) => {
        const control = event.target as HTMLInputElement;
        // A control another form owns (form="…") isn't a setting.
        if (control.form && control.form !== form) return;
        store.changed();
      };
      listen(form, 'input', changed);
      listen(form, 'change', changed);
      listen(form, 'mk-changed', () => store.changed());

      // Page scripts hear how many changes there are.
      createEffect(
        () => ({ count: store.total(), groups: store.changedCards().map(card => card.name) }),
        detail => {
          if (host) fire(host, 'mk-dirty', detail);
        },
      );

      // Before fixi (capture): a link that asks first, with changes.
      listen(document, 'click', event => {
        const link = (event.target as Element).closest?.<HTMLAnchorElement & { __go?: boolean }>('[data-leave-asks] a');
        if (!link || link.__go || link.getAttribute('aria-current') === 'page') return;
        if (store.total() === 0) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        store.setLeaving({ href: link.getAttribute('href') ?? '', place: link.textContent?.replace(/\(needs attention\)/, '').trim() ?? '' });
      }, true);

      // A link to a card, anywhere on the page (the bar, a toast, the
      // account menu).
      listen(document, 'click', event => {
        const show = (event.target as Element).closest?.('[data-show-card]');
        if (show && showCard(show.getAttribute('data-show-card') ?? '')) event.preventDefault();
      });

      // Before fixi (capture), but after a page's own scripts' capture
      // listeners, which load first: a save one of them held back isn't
      // counted as sent.
      listen(document, 'submit', event => {
        if (event.target !== form) return;
        if (store.saving()) {
          event.preventDefault();
          event.stopImmediatePropagation();
          return;
        }
        if (event.defaultPrevented) {
          store.goAfterSave = undefined;
          return;
        }
        store.setSaving(true);
        if (!form.hasAttribute('fx-action') && !store.goAfterSave) rememberPlace(form);
        // Save turns busy and disabled now, not at Solid's next update: a
        // second press as the first is sent finds it so.
        flush();
      }, true);

      listen(window, 'beforeunload', event => {
        if (!store.saving() && !store.reloading && store.total() > 0) {
          event.preventDefault();
          (event as BeforeUnloadEvent).returnValue = '';
        }
      });

      // A save that comes back without a panel to swap in (a dropped
      // connection, any failure but 422): the page guards its changes
      // again, and goes nowhere. A save that will swap is answered by the
      // form it brings.
      listen(form, 'fx:finally', event => {
        const response = (event as CustomEvent).detail?.cfg?.response as Response | undefined;
        if (response && (response.ok || response.status === 422)) return;
        store.setSaving(false);
        store.goAfterSave = undefined;
      });

      // A plain POST may come back to this page from the browser's cache.
      listen(window, 'pageshow', event => {
        if ((event as PageTransitionEvent).persisted) store.setSaving(false);
      });

      // Once its cards are in (they attach after the form), and fixi's
      // glue has put the rest of a swap's answer in place.
      const timer = setTimeout(() => {
        if (place) {
          returnTo(place);
          try {
            sessionStorage.removeItem(PLACE);
          } catch {}
        }
        if (host) settle(host, store);
        const href = previousGo.get(document);
        previousGo.delete(document);
        if (href && !store.loaded.failed && !form.querySelector('.settings-card.is-failed')) {
          const link = document.querySelector<HTMLAnchorElement & { __go?: boolean }>(`[data-leave-asks] a[href="${CSS.escape(href)}"]`);
          if (link) {
            link.__go = true;
            link.click();
            link.__go = false;
          } else {
            location.href = href;
          }
        }
      });
      onCleanup(() => {
        clearTimeout(timer);
        // "Save and go": the form fixi swaps in goes once it says the save
        // went through.
        if (store.goAfterSave) previousGo.set(document, store.goAfterSave);
      });
    },
  };
}

/** Where "Save and go" goes, handed from the form a save swaps out to the
 *  one it brings. */
const previousGo = new WeakMap<Document, string>();
