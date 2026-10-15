// The Select component's behaviour (crates/ui/src/components/select.rs):
// a button and a list that float over the page, drawn over the select the
// server sent, which stays the field. Forms, `required`, fixi and other
// scripts that read the select keep working; a script that changes the
// select (its options, `disabled`, or its value followed by an `input` or
// `change` event) is followed.
//
// The options are read from the select itself, so whatever changed it, the
// list shows what will be submitted. Each option's parts are its data
// attributes (`Choice` in Rust writes them):
//   data-label    the name shown (else the option's text); empty for an
//                 option that is only its badge (a network)
//   data-logo     a small picture before the name
//   data-detail   monospace, muted: an address, a currency code
//   data-network  a Monero network, drawn as the site's network badge
//   data-chip     a short tag; data-chip-tone="current" is plain green text
//   data-note     muted, at the end: a count, a date, why it's off
import { createMemo, createSignal, flush, onCleanup } from 'solid-js';
import type { SelectBehaviour, SelectProps } from '../../solid/Select';
import type { SelectItem } from '../../solid/types';

const SEARCH_FROM = 12;
const GAP = 4;
const TYPE_AHEAD_MS = 600;
// The list goes in the top layer where the browser has one, so nothing
// clips it: not a dialog, not a scrolling box. Elsewhere it hangs below
// the button.
const topLayer = typeof HTMLElement !== 'undefined' && typeof HTMLElement.prototype.showPopover === 'function';
let ids = 0;

// A disabled option with no value is the "Choose…" prompt: shown in the
// button until something is picked, never in the list.
const isPrompt = (option: HTMLOptionElement) => option.value === '' && option.disabled;

const networkName = (network: string) => network.charAt(0).toUpperCase() + network.slice(1);

// The name a screen reader gives the button: the select's own label,
// without the field's help or the control itself.
function nameOf(select: HTMLSelectElement): string | undefined {
  const own = select.getAttribute('aria-label');
  if (own) return own;
  const label = select.labels?.[0];
  if (!label) return undefined;
  const copy = label.cloneNode(true) as HTMLElement;
  copy.querySelectorAll('select, .mk-select, .field-help, .field-error, input, button, script').forEach(n => n.remove());
  return copy.textContent?.replace(/\s+/g, ' ').trim() || undefined;
}

export function setup(props: SelectProps): SelectBehaviour {
  const id = `mk-select-${++ids}`;
  let root: HTMLElement | undefined;
  let native: HTMLSelectElement | undefined;
  let button: HTMLElement | undefined;
  let popup: HTMLElement | undefined;
  let finder: HTMLInputElement | undefined;
  let typed = '';
  let typedAt = 0;

  // The select changed (a pick, a script, a form reset): read it again.
  // Written as the behaviour attaches (the select's `ref`), not only in
  // handlers: `ownedWrite`.
  const [version, setVersion] = createSignal(0, { ownedWrite: true });
  const reread = () => setVersion(v => v + 1);
  const [open, setOpen] = createSignal(false);
  const [query, setQuery] = createSignal('');
  const [active, setActive] = createSignal(-1);
  const [invalid, setInvalid] = createSignal(false, { ownedWrite: true });
  const [up, setUp] = createSignal(false);
  const [name, setName] = createSignal<string | undefined>(undefined, { ownedWrite: true });
  const [described, setDescribed] = createSignal<string | undefined>(undefined, { ownedWrite: true });

  const options = createMemo(() => {
    version();
    return native ? Array.from(native.options) : [];
  });
  const chosen = createMemo(() => {
    version();
    return native ? native.selectedIndex : -1;
  });

  const item = (option: HTMLOptionElement, index: number, withNote: boolean): SelectItem => {
    const data = option.dataset;
    const label = data.label ?? option.text;
    return {
      index,
      id: `${id}-option-${index}`,
      label: label || undefined,
      logo: data.logo || undefined,
      detail: data.detail || undefined,
      network: data.network ? networkName(data.network) : undefined,
      mainnet: data.network === 'mainnet',
      chip: data.chip || undefined,
      current: data.chipTone === 'current',
      note: withNote ? data.note || undefined : undefined,
      chosen: index === chosen() ? 'true' : 'false',
      active: index === active(),
      disabled: option.disabled,
    };
  };

  const searching = createMemo(() => {
    const mode = props.search ?? 'auto';
    if (mode !== 'auto') return mode === 'show';
    return options().filter(o => !isPrompt(o)).length >= SEARCH_FROM;
  });

  // The options shown: all of them, or those the find box matches.
  const matching = createMemo(() => {
    const q = query().trim().toLowerCase();
    return options()
      .map((option, index) => ({ option, index }))
      .filter(({ option }) => {
        if (isPrompt(option)) return false;
        if (!q) return true;
        const d = option.dataset;
        return [d.label ?? option.text, d.detail ?? '', d.network ?? '', d.note ?? '', option.text]
          .some(s => s.toLowerCase().includes(q));
      });
  });
  const enabled = () => matching().filter(({ option }) => !option.disabled).map(({ index }) => index);

  // Closed, nothing is drawn in the list: a page of dropdowns carries
  // nothing it doesn't show.
  const shown = createMemo(() => (open() ? matching().map(({ option, index }) => item(option, index, true)) : []));

  const current = createMemo(() => {
    const option = options()[chosen()];
    return option && !isPrompt(option) ? item(option, chosen(), false) : undefined;
  });

  // The active option scrolled into view, inside the list only.
  const point = () => {
    flush();
    const list = popup?.querySelector('.mk-list') as HTMLElement | null;
    const element = list?.querySelector<HTMLElement>(`#${CSS.escape(`${id}-option-${active()}`)}`);
    if (!list || !element) return;
    if (element.offsetTop < list.scrollTop) list.scrollTop = element.offsetTop;
    else if (element.offsetTop + element.offsetHeight > list.scrollTop + list.clientHeight) {
      list.scrollTop = element.offsetTop + element.offsetHeight - list.clientHeight;
    }
  };

  // Under the button, or above it when there's no room below and more
  // above; never past the window's edges.
  const place = () => {
    if (!popup || !button) return;
    const box = button.getBoundingClientRect();
    const below = window.innerHeight - box.bottom - GAP * 2;
    const above = box.top - GAP * 2;
    popup.style.maxHeight = '';
    const upwards = popup.offsetHeight > below && above > below;
    const room = upwards ? above : below;
    if (popup.offsetHeight > room) popup.style.maxHeight = `${Math.max(room, 120)}px`;
    setUp(upwards);
    flush();
    if (!topLayer) return;
    popup.style.minWidth = `${box.width}px`;
    const left = Math.max(GAP, Math.min(box.left, window.innerWidth - popup.offsetWidth - GAP));
    const top = upwards ? box.top - GAP - popup.offsetHeight : box.bottom + GAP;
    popup.style.left = `${left}px`;
    popup.style.top = `${top}px`;
  };

  const onOutside = (event: Event) => {
    if (root && !root.contains(event.target as Node)) close(false);
  };
  // The page or a box around the button scrolled: the list follows it,
  // unless it was the list itself scrolling.
  const onMove = (event?: Event) => {
    if (!event || !(event.target instanceof Element) || !event.target.classList.contains('mk-list')) place();
  };
  const listen = (on: boolean) => {
    const method = on ? 'addEventListener' : 'removeEventListener';
    document[method]('pointerdown', onOutside);
    window[method]('scroll', onMove, true);
    window[method]('resize', onMove);
  };
  onCleanup(() => listen(false));

  const isOpen = () => open();

  function openList() {
    if (!native || native.disabled || isOpen()) return;
    setQuery('');
    if (finder) finder.value = '';
    const start = enabled().includes(chosen()) ? chosen() : (enabled()[0] ?? -1);
    setActive(start);
    setOpen(true);
    flush();
    if (topLayer) popup?.showPopover();
    place();
    point();
    listen(true);
    finder?.focus();
  }

  function close(refocus = true) {
    if (!isOpen()) return;
    if (topLayer && popup?.matches(':popover-open')) popup.hidePopover();
    setOpen(false);
    setUp(false);
    flush();
    listen(false);
    if (refocus) button?.focus();
  }

  function choose(index: number) {
    if (!native) return;
    const option = native.options[index];
    if (!option || option.disabled) return;
    const changed = native.selectedIndex !== index;
    native.selectedIndex = index;
    close();
    if (changed) {
      native.dispatchEvent(new Event('input', { bubbles: true }));
      native.dispatchEvent(new Event('change', { bubbles: true }));
    }
    reread();
  }

  // Moves the active option by `step` enabled options, or to the first
  // (`step` = -Infinity) or last (Infinity).
  function move(step: number) {
    const list = enabled();
    if (!list.length) return;
    let at = list.indexOf(active());
    if (step === -Infinity) at = 0;
    else if (step === Infinity) at = list.length - 1;
    else at = at < 0 ? 0 : Math.max(0, Math.min(list.length - 1, at + step));
    setActive(list[at]);
    point();
  }

  // Typing jumps to the next option starting with what was typed.
  function typeAhead(char: string) {
    const now = Date.now();
    typed = now - typedAt > TYPE_AHEAD_MS ? char : typed + char;
    typedAt = now;
    const list = matching().filter(({ option }) => !option.disabled);
    const from = Math.max(0, list.findIndex(({ index }) => index === active()));
    const order = list.slice(from + (typed.length === 1 ? 1 : 0)).concat(list.slice(0, from + 1));
    const hit = order.find(({ option }) => {
      const d = option.dataset;
      return ((d.label ?? option.text) || d.network || '').toLowerCase().startsWith(typed.toLowerCase());
    });
    if (hit) {
      setActive(hit.index);
      point();
    }
  }

  function key(event: Event) {
    const e = event as KeyboardEvent;
    const inFind = e.target === finder;
    const printable = e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey;
    if (!isOpen()) {
      if (e.target !== button) return;
      if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(e.key)) {
        e.preventDefault();
        openList();
      } else if (printable && !searching()) {
        openList();
        typeAhead(e.key);
      } else if (printable) {
        openList();
      }
      return;
    }
    switch (e.key) {
      case 'ArrowDown': move(e.altKey ? Infinity : 1); break;
      case 'ArrowUp':
        if (e.altKey) { if (active() >= 0) choose(active()); else close(); break; }
        move(-1);
        break;
      case 'PageDown': move(10); break;
      case 'PageUp': move(-10); break;
      case 'Home': if (inFind) return; move(-Infinity); break;
      case 'End': if (inFind) return; move(Infinity); break;
      case 'Enter': if (active() >= 0) choose(active()); break;
      // Not the dialog around it, if any: only the list closes.
      case 'Escape': e.stopPropagation(); close(); break;
      case 'Tab': close(false); return;
      case ' ': if (inFind) return; if (active() >= 0) choose(active()); break;
      default:
        if (printable && !inFind) {
          if (finder) { finder.focus(); return; }
          typeAhead(e.key);
          break;
        }
        return;
    }
    e.preventDefault();
  }

  return {
    expanded: () => (open() ? 'true' : 'false'),
    name,
    described,
    list_id: () => `${id}-list`,
    active_id: () => (open() && active() >= 0 ? `${id}-option-${active()}` : undefined),
    invalid: () => (invalid() ? 'true' : undefined),
    disabled: () => {
      version();
      return native?.disabled ?? false;
    },
    current,
    prompt: () => {
      const option = options()[chosen()];
      return option ? option.text : '';
    },
    popover: () => (topLayer ? 'manual' : undefined),
    hidden: () => !topLayer && !open(),
    up,
    searching,
    shown,
    matches: () => !open() || matching().length > 0,
    key,
    leave(event) {
      const next = (event as FocusEvent).relatedTarget as Node | null;
      if (isOpen() && !(root && next && root.contains(next))) close(false);
    },
    toggle() {
      if (isOpen()) close();
      else openList();
    },
    find() {
      setQuery(finder?.value ?? '');
      flush();
      setActive(enabled()[0] ?? -1);
      point();
    },
    hold(event) {
      event.preventDefault();
    },
    choose,
    root(element) {
      root = element;
    },
    native(element) {
      const select = element as HTMLSelectElement;
      native = select;
      setName(nameOf(select));
      setDescribed(select.getAttribute('aria-describedby') ?? undefined);
      setInvalid(select.getAttribute('aria-invalid') === 'true');
      const onChange = () => {
        setInvalid(false);
        reread();
      };
      const onInvalid = () => setInvalid(true);
      // A browser that shows a refused required field focuses the select,
      // as does a click on its label: the button takes it.
      const onFocus = () => button?.focus();
      const onReset = () => setTimeout(reread);
      select.addEventListener('change', onChange);
      select.addEventListener('input', onChange);
      select.addEventListener('invalid', onInvalid);
      select.addEventListener('focus', onFocus);
      select.form?.addEventListener('reset', onReset);
      // A script turned options on or off, or replaced them.
      const observer = new MutationObserver(reread);
      observer.observe(select, { attributes: true, childList: true, subtree: true, characterData: true });
      onCleanup(() => {
        select.removeEventListener('change', onChange);
        select.removeEventListener('input', onChange);
        select.removeEventListener('invalid', onInvalid);
        select.removeEventListener('focus', onFocus);
        select.form?.removeEventListener('reset', onReset);
        observer.disconnect();
      });
      reread();
    },
    button(element) {
      button = element;
      // Focused before the behaviour attached (a Tab, a label's click):
      // the button takes it over.
      if (native && document.activeElement === native) queueMicrotask(() => element.focus());
    },
    popup(element) {
      popup = element;
    },
    finder(element) {
      finder = element as HTMLInputElement;
    },
  };
}
