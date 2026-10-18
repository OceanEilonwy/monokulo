// The RowsEditor component's behaviour (crates/ui/src/components/forms.rs):
// Remove marks a row at once by ticking the checkbox the save reads (the
// row's look follows that checkbox, in CSS), and pressed again puts it
// back; Add shows the blank row and puts the cursor in it.
import { createSignal } from 'solid-js';
import type { RowsEditorBehaviour, RowsEditorProps } from '../../solid/RowsEditor';

export function setup(_props: RowsEditorProps): RowsEditorBehaviour {
  const [adding, setAdding] = createSignal(false);
  let root: HTMLElement | undefined;

  return {
    adding,
    toggle(name, event) {
      const box = Array.from(root?.querySelectorAll<HTMLInputElement>('input[data-rows-remove]') ?? []).find(
        input => input.name === name,
      );
      if (!box) return;
      box.checked = !box.checked;
      (event.currentTarget as HTMLElement).setAttribute('aria-pressed', String(box.checked));
      // A settings form follows its inputs' events to mark what changed.
      box.dispatchEvent(new Event('input', { bubbles: true }));
      box.dispatchEvent(new Event('change', { bubbles: true }));
    },
    add() {
      setAdding(true);
      // Once the row shows: a hidden input can't take the cursor.
      requestAnimationFrame(() => root?.querySelector<HTMLInputElement>('.mk-rows-new input')?.focus());
    },
    root(element) {
      root = element;
    },
  };
}
