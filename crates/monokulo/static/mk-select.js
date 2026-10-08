// <mk-select>: the site's one dropdown (docs/dropdowns.md).
//
// The page sends an ordinary <select> inside <mk-select>. Without
// JavaScript that select is all there is, and every option's text says
// everything (its detail, chip and note in words). With it, this draws a
// button and a list over the select and keeps the select's value: forms,
// required fields, fixi, fx-submit-on-change and other scripts that read or
// set the select keep working, because the select is still the field.
//
// An option can carry its parts as data attributes:
//   data-label   the name shown (else the option's text)
//   data-detail  monospace, muted: an address, a currency code
//   data-chip    a short tag; data-chip-tone="current" makes it green
//   data-note    muted, at the end: a count, a date, why it's off
//
// Attributes on <mk-select>:
//   compact             the toolbar size
//   search="auto|show|hide"
//                       a find box at the top of the list; auto (the
//                       default) shows it from 12 options up
(() => {
  'use strict';

  if (!window.customElements || customElements.get('mk-select')) return;

  const SEARCH_FROM = 12;
  const GAP = 4;
  // The list goes in the top layer where the browser has one, so nothing
  // clips it: not a dialog, not a scrolling box. Elsewhere it hangs below
  // the button (`.mk-popup` in site.css).
  const topLayer = typeof HTMLElement.prototype.showPopover === 'function';
  const TYPE_AHEAD_MS = 600;
  const value = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'value');
  const selectedIndex = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, 'selectedIndex');
  let ids = 0;

  const el = (tag, className, text) => {
    const node = document.createElement(tag);
    if (className) node.className = className;
    if (text) node.textContent = text;
    return node;
  };

  // A disabled option with no value is the "Choose…" prompt: shown in the
  // button until something is picked, never in the list.
  const isPrompt = (option) => option.value === '' && option.disabled;

  // An option's parts, read from the select each time.
  const parts = (option) => ({
    label: option.dataset.label || option.text,
    detail: option.dataset.detail || '',
    chip: option.dataset.chip || '',
    current: option.dataset.chipTone === 'current',
    note: option.dataset.note || '',
  });

  // The name a screen reader gives the button: the select's own label,
  // without the field's help or the control itself.
  const nameOf = (select) => {
    const own = select.getAttribute('aria-label');
    if (own) return own;
    const label = select.labels && select.labels[0];
    if (!label) return '';
    const copy = label.cloneNode(true);
    copy.querySelectorAll('select, mk-select, .field-help, .field-error, input, button').forEach((n) => n.remove());
    return copy.textContent.replace(/\s+/g, ' ').trim();
  };

  class MkSelect extends HTMLElement {
    connectedCallback() {
      if (this.button) return;
      const select = this.querySelector(':scope > select');
      if (!select) return;
      this.select = select;
      this.id ||= `mk-select-${++ids}`;
      this.typed = '';
      this.typedAt = 0;
      this.active = -1;

      const button = el('button', 'mk-button');
      button.type = 'button';
      button.setAttribute('role', 'combobox');
      button.setAttribute('aria-haspopup', 'listbox');
      button.setAttribute('aria-expanded', 'false');
      const name = nameOf(select);
      if (name) button.setAttribute('aria-label', name);
      const described = select.getAttribute('aria-describedby');
      if (described) button.setAttribute('aria-describedby', described);
      this.shown = el('span', 'mk-value');
      button.append(this.shown, el('span', 'mk-caret'));
      this.button = button;
      select.after(button);

      // The select stays the field, out of sight and out of the tab order.
      select.classList.add('mk-native');
      select.tabIndex = -1;
      select.setAttribute('aria-hidden', 'true');

      // Other scripts set the select's value or turn options off; follow.
      const element = this;
      for (const [prop, descriptor] of [['value', value], ['selectedIndex', selectedIndex]]) {
        Object.defineProperty(select, prop, {
          configurable: true,
          get() { return descriptor.get.call(this); },
          set(next) { descriptor.set.call(this, next); element.sync(); },
        });
      }
      this.observer = new MutationObserver(() => this.sync());
      this.observer.observe(select, { attributes: true, childList: true, subtree: true, characterData: true });

      select.addEventListener('change', () => { this.setInvalid(false); this.sync(); });
      select.addEventListener('invalid', () => this.setInvalid(true));
      // A browser that shows a refused required field focuses the select,
      // as does a click on its label: the button takes it.
      select.addEventListener('focus', () => button.focus());
      if (select.form) {
        this.onReset = () => setTimeout(() => this.sync());
        select.form.addEventListener('reset', this.onReset);
      }

      button.addEventListener('click', () => (this.isOpen() ? this.close() : this.open()));
      this.addEventListener('keydown', (event) => this.key(event));
      this.addEventListener('focusout', (event) => {
        if (this.isOpen() && !this.contains(event.relatedTarget)) this.close(false);
      });
      this.onOutside = (event) => { if (!this.contains(event.target)) this.close(false); };
      // The page or a box around the button scrolled: the list follows it,
      // unless it was the list itself scrolling.
      this.onMove = (event) => { if (!event || event.target !== this.list) this.place(); };

      if (select.getAttribute('aria-invalid') === 'true') this.setInvalid(true);
      this.sync();
    }

    disconnectedCallback() {
      if (this.observer) this.observer.disconnect();
      document.removeEventListener('pointerdown', this.onOutside);
      window.removeEventListener('scroll', this.onMove, true);
      window.removeEventListener('resize', this.onMove);
    }

    // The button says what the select holds; the list, when open, too.
    sync() {
      const select = this.select;
      const option = select.options[selectedIndex.get.call(select)];
      this.shown.replaceChildren();
      if (!option || isPrompt(option)) {
        this.shown.append(el('span', 'mk-prompt', option ? option.text : ''));
      } else {
        this.shown.append(...this.draw(parts(option), false));
      }
      this.button.disabled = select.disabled;
      if (this.isOpen()) this.fill();
    }

    draw(p, withNote) {
      const out = [el('span', 'mk-label', p.label)];
      if (p.detail) out.push(el('span', 'mk-detail', p.detail));
      if (p.chip) out.push(el('span', `tag ${p.current ? 'tag-ok' : 'tag-unknown'} mk-chip`, p.chip));
      if (withNote && p.note) out.push(el('span', 'mk-note', p.note));
      return out;
    }

    setInvalid(on) {
      this.classList.toggle('mk-invalid', on);
      if (on) this.button.setAttribute('aria-invalid', 'true');
      else this.button.removeAttribute('aria-invalid');
    }

    searching() {
      const mode = this.getAttribute('search') || 'auto';
      if (mode === 'show') return true;
      if (mode === 'hide') return false;
      return Array.from(this.select.options).filter((o) => !isPrompt(o)).length >= SEARCH_FROM;
    }

    isOpen() {
      return !!this.popup && this.classList.contains('mk-open');
    }

    // The list is built on first opening, so a page that never opens one
    // carries nothing extra.
    build() {
      const popup = el('div', 'mk-popup');
      if (topLayer) popup.popover = 'manual';
      else popup.hidden = true;
      const list = el('ul', 'mk-list');
      list.id = `${this.id}-list`;
      list.setAttribute('role', 'listbox');
      if (this.button.hasAttribute('aria-label')) list.setAttribute('aria-label', this.button.getAttribute('aria-label'));
      list.addEventListener('mousedown', (event) => event.preventDefault());
      list.addEventListener('click', (event) => {
        const item = event.target.closest('[role=option]');
        if (item && item.getAttribute('aria-disabled') !== 'true') this.choose(Number(item.dataset.index));
      });
      this.button.setAttribute('aria-controls', list.id);
      if (this.searching()) {
        const find = el('input', 'mk-find');
        find.type = 'search';
        find.placeholder = 'Type to find…';
        find.autocomplete = 'off';
        find.setAttribute('aria-label', 'Find');
        find.setAttribute('role', 'combobox');
        find.setAttribute('aria-autocomplete', 'list');
        find.setAttribute('aria-expanded', 'true');
        find.setAttribute('aria-controls', list.id);
        find.addEventListener('input', () => { this.active = -1; this.fill(); });
        this.find = find;
        popup.append(find);
      }
      this.empty = el('p', 'mk-empty', 'Nothing matches.');
      this.empty.setAttribute('role', 'status');
      this.empty.hidden = true;
      popup.append(list, this.empty);
      this.list = list;
      this.popup = popup;
      this.append(popup);
    }

    // The options shown: all of them, or those the find box matches.
    shownOptions() {
      const query = this.find ? this.find.value.trim().toLowerCase() : '';
      return Array.from(this.select.options)
        .map((option, index) => ({ option, index, p: parts(option) }))
        .filter(({ option, p }) => !isPrompt(option) &&
          (!query || [p.label, p.detail, p.note, option.text].some((s) => s.toLowerCase().includes(query))));
    }

    fill() {
      const shown = this.shownOptions();
      const chosen = selectedIndex.get.call(this.select);
      if (!shown.some((s) => s.index === this.active && !s.option.disabled)) {
        const start = shown.find((s) => s.index === chosen && !s.option.disabled) || shown.find((s) => !s.option.disabled);
        this.active = start ? start.index : -1;
      }
      this.list.replaceChildren(...shown.map(({ option, index, p }) => {
        const item = el('li', 'mk-option');
        item.id = `${this.id}-option-${index}`;
        item.dataset.index = index;
        item.setAttribute('role', 'option');
        item.setAttribute('aria-selected', index === chosen ? 'true' : 'false');
        if (option.disabled) item.setAttribute('aria-disabled', 'true');
        if (index === this.active) item.classList.add('mk-active');
        item.append(...this.draw(p, true));
        return item;
      }));
      this.empty.hidden = shown.length > 0;
      this.point();
    }

    // aria-activedescendant on whichever has focus, and the active option
    // scrolled into view.
    point() {
      const item = this.active >= 0 ? this.list.querySelector(`#${CSS.escape(`${this.id}-option-${this.active}`)}`) : null;
      for (const owner of [this.button, this.find]) {
        if (!owner) continue;
        if (item) owner.setAttribute('aria-activedescendant', item.id);
        else owner.removeAttribute('aria-activedescendant');
      }
      this.list.querySelectorAll('.mk-active').forEach((n) => n.classList.remove('mk-active'));
      if (item) {
        item.classList.add('mk-active');
        // Scrolls the list only, never the page or a dialog around it.
        const list = this.list;
        if (item.offsetTop < list.scrollTop) list.scrollTop = item.offsetTop;
        else if (item.offsetTop + item.offsetHeight > list.scrollTop + list.clientHeight) {
          list.scrollTop = item.offsetTop + item.offsetHeight - list.clientHeight;
        }
      }
    }

    open() {
      if (this.select.disabled || this.isOpen()) return;
      if (!this.popup) this.build();
      if (this.find) this.find.value = '';
      this.active = -1;
      this.classList.add('mk-open');
      if (topLayer) this.popup.showPopover();
      else this.popup.hidden = false;
      this.button.setAttribute('aria-expanded', 'true');
      this.fill();
      this.place();
      this.point();
      document.addEventListener('pointerdown', this.onOutside);
      window.addEventListener('scroll', this.onMove, true);
      window.addEventListener('resize', this.onMove);
      if (this.find) this.find.focus();
    }

    // Under the button, or above it when there's no room below and more
    // above; never past the window's edges.
    place() {
      const popup = this.popup;
      const box = this.button.getBoundingClientRect();
      const below = window.innerHeight - box.bottom - GAP * 2;
      const above = box.top - GAP * 2;
      popup.style.maxHeight = '';
      const up = popup.offsetHeight > below && above > below;
      const room = up ? above : below;
      if (popup.offsetHeight > room) popup.style.maxHeight = `${Math.max(room, 120)}px`;
      this.classList.toggle('mk-up', up);
      if (!topLayer) return;
      popup.style.minWidth = `${box.width}px`;
      const left = Math.max(GAP, Math.min(box.left, window.innerWidth - popup.offsetWidth - GAP));
      const top = up ? box.top - GAP - popup.offsetHeight : box.bottom + GAP;
      popup.style.left = `${left}px`;
      popup.style.top = `${top}px`;
    }

    close(refocus = true) {
      if (!this.isOpen()) return;
      if (topLayer) this.popup.hidePopover();
      else this.popup.hidden = true;
      this.button.setAttribute('aria-expanded', 'false');
      this.button.removeAttribute('aria-activedescendant');
      this.classList.remove('mk-open', 'mk-up');
      document.removeEventListener('pointerdown', this.onOutside);
      window.removeEventListener('scroll', this.onMove, true);
      window.removeEventListener('resize', this.onMove);
      if (refocus) this.button.focus();
    }

    choose(index) {
      const select = this.select;
      const changed = selectedIndex.get.call(select) !== index;
      selectedIndex.set.call(select, index);
      this.close();
      if (changed) {
        select.dispatchEvent(new Event('input', { bubbles: true }));
        select.dispatchEvent(new Event('change', { bubbles: true }));
      }
      this.sync();
    }

    // Moves the active option by `step` enabled options, or to the first
    // (`step` = -Infinity) or last (Infinity).
    move(step) {
      const enabled = this.shownOptions().filter((s) => !s.option.disabled).map((s) => s.index);
      if (!enabled.length) return;
      let at = enabled.indexOf(this.active);
      if (step === -Infinity) at = 0;
      else if (step === Infinity) at = enabled.length - 1;
      else at = at < 0 ? 0 : Math.max(0, Math.min(enabled.length - 1, at + step));
      this.active = enabled[at];
      this.point();
    }

    // Typing jumps to the next option starting with what was typed.
    typeAhead(char) {
      const now = Date.now();
      this.typed = now - this.typedAt > TYPE_AHEAD_MS ? char : this.typed + char;
      this.typedAt = now;
      const enabled = this.shownOptions().filter((s) => !s.option.disabled);
      const from = Math.max(0, enabled.findIndex((s) => s.index === this.active));
      const order = enabled.slice(from + (this.typed.length === 1 ? 1 : 0)).concat(enabled.slice(0, from + 1));
      const hit = order.find((s) => s.p.label.toLowerCase().startsWith(this.typed.toLowerCase()));
      if (hit) {
        this.active = hit.index;
        this.point();
      }
    }

    key(event) {
      const inFind = event.target === this.find;
      const printable = event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey;
      if (!this.isOpen()) {
        if (event.target !== this.button) return;
        if (['ArrowDown', 'ArrowUp', 'Enter', ' '].includes(event.key)) {
          event.preventDefault();
          this.open();
        } else if (printable && !this.searching()) {
          this.open();
          this.typeAhead(event.key);
        } else if (printable) {
          this.open();
        }
        return;
      }
      switch (event.key) {
        case 'ArrowDown': this.move(event.altKey ? Infinity : 1); break;
        case 'ArrowUp':
          if (event.altKey) { if (this.active >= 0) this.choose(this.active); else this.close(); break; }
          this.move(-1);
          break;
        case 'PageDown': this.move(10); break;
        case 'PageUp': this.move(-10); break;
        case 'Home': if (inFind) return; this.move(-Infinity); break;
        case 'End': if (inFind) return; this.move(Infinity); break;
        case 'Enter': if (this.active >= 0) this.choose(this.active); break;
        // Not the dialog around it, if any: only the list closes.
        case 'Escape': event.stopPropagation(); this.close(); break;
        case 'Tab': this.close(false); return;
        case ' ': if (inFind) return; if (this.active >= 0) this.choose(this.active); break;
        default:
          if (printable && !inFind) {
            if (this.find) { this.find.focus(); return; }
            this.typeAhead(event.key);
            break;
          }
          return;
      }
      event.preventDefault();
    }
  }

  customElements.define('mk-select', MkSelect);
})();
