// The confirm dialogs: a wallet's page (Retire, Restore:
// `views::wallets::detail_page`) and a store's settings (change or remove
// its website, disconnect a plugin: `views::store_site`).
//
// Every "…" button is a link to a page holding the same content as its
// dialog; with JavaScript the dialog, already on the page, opens instead.
// Cancel and Close are links back, and here close the dialog.
//
// A store's settings: saving the settings form with the website changed
// (or emptied) from a saved one opens the change (or remove) dialog with
// the new address, instead of saving it; the rest of the form stays as
// typed. Without JavaScript the server answers that save with the page.
(() => {
  'use strict';

  if (typeof HTMLDialogElement !== 'function') return;

  const open = (dialog) => {
    dialog.showModal();
    const first = dialog.querySelector('input:not([type=hidden]):not([disabled])');
    if (first) first.focus();
  };

  document.addEventListener('click', (event) => {
    if (!(event.target instanceof Element)) return;
    const opener = event.target.closest('[data-opens-dialog]');
    if (opener) {
      const dialog = document.getElementById(opener.dataset.opensDialog);
      if (!dialog || typeof dialog.showModal !== 'function') return;
      event.preventDefault();
      const from = opener.closest('dialog');
      if (from && from !== dialog) from.close();
      open(dialog);
      return;
    }
    const closer = event.target.closest('[data-closes-dialog]');
    const dialog = closer && closer.closest('dialog');
    if (dialog) {
      event.preventDefault();
      dialog.close();
    }
  });

  // A dialog closed by any means gives focus back to what opened it.
  document.addEventListener('close', (event) => {
    const dialog = event.target;
    if (!(dialog instanceof HTMLDialogElement) || !dialog.id) return;
    const opener = document.querySelector(`[data-opens-dialog="${CSS.escape(dialog.id)}"]`);
    if (opener) opener.focus();
  }, true);

  // The store's website, changed in the settings form: asked first.
  document.addEventListener('submit', (event) => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement)) return;
    const site = form.querySelector('input[data-site-saved]');
    if (!site) return;
    const saved = site.dataset.siteSaved.trim();
    const typed = site.value.trim();
    if (!saved || typed === saved) return;
    const dialog = document.getElementById(typed ? 'website-dialog' : 'website-remove-dialog');
    if (!dialog || typeof dialog.showModal !== 'function') return;
    event.preventDefault();
    event.stopImmediatePropagation();
    const field = dialog.querySelector('input[name="site"]');
    if (field) field.value = typed;
    open(dialog);
  }, true);
})();
