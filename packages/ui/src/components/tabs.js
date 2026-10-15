// Anchors and visible sections are the fallback. Upgrade only a complete local set.
if (!customElements.get('mk-tabs')) customElements.define('mk-tabs', class extends HTMLElement {
  connectedCallback() {
    if (this.upgraded) return;
    const list = this.querySelector('.mk-tabs');
    if (!list) return;
    const tabs = [...list.querySelectorAll('a[href^="#"]')];
    const panels = tabs.map(tab => document.getElementById(tab.hash.slice(1)));
    if (!tabs.length || panels.some(panel => !panel || !this.contains(panel))) return;
    this.upgraded = true;
    list.setAttribute('role', 'tablist');
    const activate = index => tabs.forEach((tab, i) => {
      tab.setAttribute('aria-selected', String(i === index));
      tab.tabIndex = i === index ? 0 : -1;
      panels[i].hidden = i !== index;
    });
    tabs.forEach((tab, i) => {
      tab.id ||= `${panels[i].id}-tab`;
      tab.setAttribute('role', 'tab');
      tab.setAttribute('aria-controls', panels[i].id);
      panels[i].setAttribute('role', 'tabpanel');
      panels[i].setAttribute('aria-labelledby', tab.id);
      panels[i].tabIndex = 0;
      tab.addEventListener('click', event => { event.preventDefault(); activate(i); });
      tab.addEventListener('keydown', event => {
        let next;
        if (event.key === 'ArrowRight') next = (i + 1) % tabs.length;
        if (event.key === 'ArrowLeft') next = (i + tabs.length - 1) % tabs.length;
        if (event.key === 'Home') next = 0;
        if (event.key === 'End') next = tabs.length - 1;
        if (next !== undefined) { event.preventDefault(); activate(next); tabs[next].focus(); }
      });
    });
    const linked = tabs.findIndex(tab => tab.hash === location.hash);
    activate(linked < 0 ? 0 : linked);
  }
});
