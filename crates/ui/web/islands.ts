// Islands: where an interactive component's behaviour attaches to the
// markup the server rendered (crates/ui). The page works without this;
// with it, each behaviour adds what only it draws (`@if enhanced`) at the
// server's markers and binds its handlers, leaving the server's elements,
// and so their values, focus and other scripts' listeners, in place.
//
// An island is the component's root element: `data-island` names the
// component, `data-props` holds its props, `data-mount` says when it
// attaches: at once (absent), "visible" when it first scrolls into view,
// "interaction" on its first pointer, focus or key. One observer attaches
// what fixi or a script adds to the page and lets go of what they take
// away. Islands inside another are attached on their own, after it: an
// island's behaviour never renders what's inside its slots.
import { islands } from '../solid/islands';

const INTERACTION = ['pointerdown', 'focusin', 'keydown'] as const;
const attached = new WeakMap<Element, () => void>();
const waiting = new WeakMap<Element, () => void>();

function attach(element: HTMLElement) {
  if (attached.has(element) || waiting.has(element)) return;
  const name = element.dataset.island ?? '';
  const start = islands[name];
  if (!start) {
    console.error(`[data-island="${name}"]: no such interactive component`);
    return;
  }
  const go = () => {
    waiting.get(element)?.();
    waiting.delete(element);
    if (attached.has(element) || !element.isConnected) return;
    attached.set(element, start(element, JSON.parse(element.dataset.props ?? '{}') as never));
  };
  switch (element.dataset.mount) {
    case 'visible': {
      if (!('IntersectionObserver' in window)) return go();
      const observer = new IntersectionObserver(entries => {
        if (entries.some(entry => entry.isIntersecting)) go();
      });
      observer.observe(element);
      waiting.set(element, () => observer.disconnect());
      return;
    }
    case 'interaction': {
      for (const event of INTERACTION) element.addEventListener(event, go, { once: true, capture: true });
      waiting.set(element, () => { for (const event of INTERACTION) element.removeEventListener(event, go, { capture: true }); });
      return;
    }
    default:
      go();
  }
}

function release(element: Element) {
  // Moved within the page, it is back already: only an island that stays
  // out lets go.
  if (element.isConnected) return;
  waiting.get(element)?.();
  waiting.delete(element);
  attached.get(element)?.();
  attached.delete(element);
}

const within = (node: Node): HTMLElement[] => {
  if (!(node instanceof HTMLElement)) return [];
  const found = Array.from(node.querySelectorAll<HTMLElement>('[data-island]'));
  return node.matches('[data-island]') ? [node, ...found] : found;
};

new MutationObserver(records => {
  for (const record of records) {
    for (const node of Array.from(record.removedNodes)) within(node).forEach(release);
    for (const node of Array.from(record.addedNodes)) within(node).forEach(attach);
  }
}).observe(document.documentElement, { childList: true, subtree: true });

within(document.documentElement).forEach(attach);
