// `<mk-island>`: where an interactive component's behaviour attaches to the
// markup the server rendered (crates/ui). The page works without it; with
// it, the behaviour adds what only it draws (`@if enhanced`) at the
// server's markers and binds its handlers, leaving the server's elements,
// and so their values, focus and other scripts' listeners, in place.
//
// The one custom element: its connect and disconnect are each island's
// mount and dispose, so markup fixi swaps in attaches by itself and
// markup it swaps out lets go. An island inside another is the outer
// one's to render.
//
// mount="load" attaches at once; "visible" when it first scrolls into
// view; "interaction" on its first pointer, focus or key.
import { islands } from '../solid/islands';

const INTERACTION = ['pointerdown', 'focusin', 'keydown'] as const;

class MkIsland extends HTMLElement {
  private dispose?: () => void;
  private waiting?: () => void;

  connectedCallback() {
    if (this.dispose || this.waiting || this.parentElement?.closest('mk-island')) return;
    const attach = () => {
      this.waiting?.();
      this.waiting = undefined;
      this.attach();
    };
    switch (this.getAttribute('mount')) {
      case 'visible': {
        if (!('IntersectionObserver' in window)) return attach();
        const observer = new IntersectionObserver(entries => {
          if (entries.some(entry => entry.isIntersecting)) attach();
        });
        // `display: contents` has no box: watch what's inside.
        for (const child of this.children) if (child.localName !== 'script') observer.observe(child);
        this.waiting = () => observer.disconnect();
        return;
      }
      case 'interaction': {
        for (const event of INTERACTION) this.addEventListener(event, attach, { once: true, capture: true });
        this.waiting = () => { for (const event of INTERACTION) this.removeEventListener(event, attach, { capture: true }); };
        return;
      }
      default:
        attach();
    }
  }

  disconnectedCallback() {
    // Moved within the page, it is connected again straight away: only an
    // island that stays out lets go.
    queueMicrotask(() => {
      if (this.isConnected) return;
      this.waiting?.();
      this.waiting = undefined;
      this.dispose?.();
      this.dispose = undefined;
    });
  }

  private attach() {
    if (this.dispose || !this.isConnected) return;
    const name = this.getAttribute('component') ?? '';
    const attach = islands[name];
    const json = this.querySelector(':scope > script[data-props]')?.textContent;
    if (!attach || json == null) {
      console.error(`<mk-island component="${name}">: no such interactive component, or no props`);
      return;
    }
    this.dispose = attach(this, JSON.parse(json) as never);
  }
}

if (!customElements.get('mk-island')) customElements.define('mk-island', MkIsland);
