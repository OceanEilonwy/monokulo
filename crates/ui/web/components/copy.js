import { truncateValue } from './truncate.js';
// CSS truncates only the presentation. The complete text remains the source of truth.
if (!customElements.get('mk-copy')) customElements.define('mk-copy', class extends HTMLElement {
  connectedCallback() {
    if (this.button) { this.observe(); return; }
    const value = this.querySelector('[data-copy-value]');
    if (!value) return;
    this.value = value;
    this.preview = document.createElement('span');
    this.preview.className = 'mk-copy-preview mk-mono';
    this.preview.setAttribute('aria-hidden', 'true');
    this.append(this.preview);
    this.preview.addEventListener('click', () => {
      value.focus();
      const range = document.createRange();
      range.selectNodeContents(value);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
    });
    this.button = document.createElement('button');
    this.button.type = 'button';
    this.button.className = 'mk-btn';
    this.button.dataset.size = 'compact';
    this.button.textContent = 'Copy';
    this.button.setAttribute('aria-label', this.getAttribute('copy-label') || 'Copy full value');
    const status = document.createElement('span');
    status.className = 'mk-copy-status';
    status.setAttribute('role', 'status');
    this.append(this.button, status);
    this.observe();
    this.button.addEventListener('click', async () => {
      this.button.disabled = true;
      status.textContent = '';
      // Read at click time: reactive renderers can update the value in place.
      const text = value.textContent;
      try {
        if (!navigator.clipboard?.writeText) throw new Error('Clipboard unavailable');
        await navigator.clipboard.writeText(text);
        status.textContent = 'Copied';
      } catch {
        // Leave the actual text selected for a manual copy; never claim success.
        value.focus();
        const range = document.createRange();
        range.selectNodeContents(value);
        const selection = window.getSelection();
        selection.removeAllRanges();
        selection.addRange(range);
        status.textContent = 'Select and copy the full value using your device’s copy command.';
      } finally {
        this.button.disabled = false;
      }
    });
  }
  observe() {
    const update = () => {
      const full = this.value.textContent;
      const shown = truncateValue(full, this.getAttribute('truncate'), this.getAttribute('segments'), Number(this.getAttribute('head')), Number(this.getAttribute('tail')));
      this.preview.textContent = shown;
      this.toggleAttribute('data-truncated', shown !== full);
      this.preview.hidden = shown === full;
    };
    this.observer = new MutationObserver(update);
    this.observer.observe(this.value, { childList: true, subtree: true, characterData: true });
    this.observer.observe(this, { attributes: true, attributeFilter: ['truncate', 'segments', 'head', 'tail'] });
    update();
  }
  disconnectedCallback() { this.observer?.disconnect(); }
});
