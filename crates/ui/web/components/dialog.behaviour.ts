// A dialog's behaviour (crates/ui/src/components/dialog.rs): the links and
// buttons naming it in `data-opens-dialog` open it, rather than going to
// its page; `data-closes-dialog` inside it closes it, rather than going
// back; and focus goes to its first field, then back to what opened it.
//
// With `confirms`, a form's save with that field changed from what was
// saved (or emptied) opens it instead of saving: the rest of the form stays
// as typed, and the dialog's field marked `data-confirmed` gets what was
// typed.
import { onCleanup } from 'solid-js';
import type { DialogBehaviour, DialogProps } from '../../solid/Dialog';

export function setup(props: DialogProps): DialogBehaviour {
  return {
    root(element) {
      const dialog = element as HTMLDialogElement;
      if (typeof dialog.showModal !== 'function') return;
      let opener: HTMLElement | null = null;

      const open = (from: HTMLElement | null) => {
        opener = from;
        if (!dialog.open) dialog.showModal();
        dialog.querySelector<HTMLElement>('input:not([type=hidden]):not([disabled])')?.focus();
      };

      const click = (event: Event) => {
        const target = event.target as Element;
        const opening = target.closest?.<HTMLElement>('[data-opens-dialog]');
        if (opening && opening.dataset.opensDialog === props.id) {
          event.preventDefault();
          // Opened from another dialog: that one gives way.
          const from = opening.closest('dialog');
          if (from && from !== dialog) from.close();
          open(opening);
          return;
        }
        const closing = target.closest?.('[data-closes-dialog]');
        if (closing && closing.closest('dialog') === dialog) {
          event.preventDefault();
          dialog.close();
        }
      };

      // Closed any way (Escape too): focus back on what opened it.
      const closed = () => {
        const back = opener ?? document.querySelector<HTMLElement>(`[data-opens-dialog="${CSS.escape(props.id)}"]`);
        opener = null;
        back?.focus();
      };

      // In the capture phase on the window: before the form's own
      // listeners, so a save asked about first isn't counted as sent.
      const submit = (event: Event) => {
        if (!props.confirms) return;
        const form = event.target as HTMLFormElement;
        const field = form.elements?.namedItem?.(props.confirms);
        if (!(field instanceof HTMLInputElement) || field.dataset.saved === undefined) return;
        const saved = field.dataset.saved.trim();
        const typed = field.value.trim();
        if (!saved || typed === saved) return;
        if ((props.confirms_when ?? 'changed') === 'emptied' ? typed !== '' : typed === '') return;
        event.preventDefault();
        event.stopImmediatePropagation();
        const own = dialog.querySelector<HTMLInputElement>('input[data-confirmed]');
        if (own) own.value = typed;
        open(null);
      };

      document.addEventListener('click', click);
      dialog.addEventListener('close', closed);
      window.addEventListener('submit', submit, true);
      onCleanup(() => {
        document.removeEventListener('click', click);
        dialog.removeEventListener('close', closed);
        window.removeEventListener('submit', submit, true);
      });
    },
  };
}
