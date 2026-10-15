// A sortable list's behaviour (crates/ui/src/components/sortable.rs): an
// item follows the pointer from its handle, the items it passes sliding
// out of its way, and settles into its slot when let go; the handle's up
// and down arrows move it a place. Fixed items stay at the end. Each move
// sends `mk-reorder`; a new order once settled sends `mk-reordered`.
import { onCleanup } from 'solid-js';
import type { SortableListBehaviour, SortableListProps } from '../../solid/SortableList';

type Drag = { item: HTMLElement; startNext: Element | null; grab: number; offset: number };

export function setup(_props: SortableListProps): SortableListBehaviour {
  return {
    root(element) {
      const list = element as HTMLElement;
      const calm = window.matchMedia?.('(prefers-reduced-motion: reduce)');
      let drag: Drag | null = null;

      const items = () => Array.from(list.children) as HTMLElement[];
      const itemOf = (target: EventTarget | null) => {
        const handle = (target as Element | null)?.closest?.('[data-sortable-handle]');
        if (!handle || !list.contains(handle)) return null;
        const item = handle.closest('.mk-sortable > *') as HTMLElement | null;
        return item && item.parentElement === list ? { handle: handle as HTMLElement, item } : null;
      };
      const tell = (name: string, item: HTMLElement) =>
        list.dispatchEvent(new CustomEvent(name, { bubbles: true, detail: { item } }));

      const measure = () => new Map(items().map(item => [item, item.getBoundingClientRect().top]));
      // The items but `except` slide from where they were to where they are.
      const slide = (except: HTMLElement, tops: Map<HTMLElement, number>) => {
        if (calm?.matches) return;
        for (const item of items()) {
          const was = tops.get(item);
          if (item === except || was === undefined) continue;
          const by = was - item.getBoundingClientRect().top;
          if (!by) continue;
          item.style.transition = 'none';
          item.style.transform = `translateY(${by}px)`;
          item.getBoundingClientRect();
          item.style.transition = '';
          item.style.transform = '';
        }
      };
      const move = (item: HTMLElement, before: Element | null) => {
        const tops = measure();
        list.insertBefore(item, before);
        slide(item, tops);
        tell('mk-reorder', item);
      };
      // Where the dragged item belongs for a pointer at `y`: before the
      // first other item whose middle is below it, never after the fixed.
      const slotFor = (item: HTMLElement, y: number) => {
        for (const other of items()) {
          if (other === item) continue;
          if (other.hasAttribute('data-sortable-fixed')) return other;
          const box = other.getBoundingClientRect();
          if (y < box.top + box.height / 2) return other;
        }
        return null;
      };

      const down = (event: PointerEvent) => {
        const at = itemOf(event.target);
        if (!at || event.button !== 0) return;
        event.preventDefault();
        drag = { item: at.item, startNext: at.item.nextElementSibling, grab: event.clientY - at.item.getBoundingClientRect().top, offset: 0 };
        at.item.classList.add('is-dragging');
        at.handle.setPointerCapture(event.pointerId);
      };
      const moveTo = (event: PointerEvent) => {
        if (!drag) return;
        const before = slotFor(drag.item, event.clientY);
        if (before !== drag.item.nextElementSibling) move(drag.item, before);
        const slotTop = drag.item.getBoundingClientRect().top - drag.offset;
        drag.offset = event.clientY - drag.grab - slotTop;
        drag.item.style.transform = `translateY(${drag.offset}px)`;
      };
      const end = (cancelled: boolean) => {
        if (!drag) return;
        const { item, startNext } = drag;
        drag = null;
        if (cancelled && item.nextElementSibling !== startNext) move(item, startNext);
        item.classList.remove('is-dragging');
        item.style.transform = '';
        if (item.nextElementSibling !== startNext) tell('mk-reordered', item);
      };
      const key = (event: KeyboardEvent) => {
        if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') return;
        const at = itemOf(event.target);
        if (!at) return;
        event.preventDefault();
        const { item } = at;
        if (event.key === 'ArrowUp') {
          const above = item.previousElementSibling;
          if (!above) return;
          move(item, above);
        } else {
          const below = item.nextElementSibling;
          if (!below || below.hasAttribute('data-sortable-fixed')) return;
          move(item, below.nextElementSibling);
        }
        tell('mk-reordered', item);
        item.querySelector<HTMLElement>('[data-sortable-handle]')?.focus();
      };
      // The handle drags; it doesn't open its row.
      const click = (event: Event) => {
        if (itemOf(event.target)) event.preventDefault();
      };
      const up = () => end(false);
      const cancel = () => end(true);

      list.addEventListener('pointerdown', down);
      document.addEventListener('pointermove', moveTo);
      document.addEventListener('pointerup', up);
      document.addEventListener('pointercancel', cancel);
      list.addEventListener('keydown', key);
      list.addEventListener('click', click);
      onCleanup(() => {
        list.removeEventListener('pointerdown', down);
        document.removeEventListener('pointermove', moveTo);
        document.removeEventListener('pointerup', up);
        document.removeEventListener('pointercancel', cancel);
        list.removeEventListener('keydown', key);
        list.removeEventListener('click', click);
      });
    },
  };
}
