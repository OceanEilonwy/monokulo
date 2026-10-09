// A wallet's page (crates/monokulo/src/views/wallets.rs, `detail_page`).
// "Retire wallet…" and "Restore wallet…" are links to pages holding the
// retire checklist and the restore form; with JavaScript they open the
// same content, already on the page, as a dialog. Cancel and Close are
// links back to the wallet's page, and here close the dialog.
(() => {
  'use strict';

  if (typeof HTMLDialogElement !== 'function') return;

  document.addEventListener('click', (event) => {
    if (!(event.target instanceof Element)) return;
    const opener = event.target.closest('[data-opens-dialog]');
    if (opener) {
      const dialog = document.getElementById(opener.dataset.opensDialog);
      if (!dialog || typeof dialog.showModal !== 'function') return;
      event.preventDefault();
      dialog.showModal();
      const first = dialog.querySelector('input:not([type=hidden]):not([disabled])');
      if (first) first.focus();
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
})();
