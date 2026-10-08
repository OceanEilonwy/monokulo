// Enhance the existing server-rendered sections; without JS the forms stay inline.
document.addEventListener('DOMContentLoaded', () => {
  const root = document.querySelector('[data-store-settings]');
  if (!root || typeof HTMLDialogElement === 'undefined') return;
  let active = null;
  let submitted = null;
  const summaries = {
    'base-currency': section => section.querySelector('[name=base_currency]')?.value,
    'confirmation-thresholds': section => `${section.querySelector('[name=confirmations_required]')?.value || '0'} confirmations by default`,
    'fx-provider': section => Array.from(section.querySelectorAll('input[name^=use_]:checked')).map(input => input.name.slice(4)).join(', ') || 'No exchange rate providers',
    'key-storage': section => section.querySelector('p')?.textContent,
    'verified-domains': section => `${section.querySelectorAll('tbody tr').length} domain entries`,
    'webhooks': section => `${section.querySelectorAll('tbody tr').length} webhooks`,
    'diagnostics': section => section.querySelector('input[type=checkbox]')?.checked ? 'Client logging enabled' : 'Client logging disabled'
  };
  function enhance() {
    for (const section of root.querySelectorAll(':scope > section')) {
      // A section shown in place (the store's wallet) stays as it is.
      if (section.dataset.dialogReady || !section.querySelector('h2') || section.hasAttribute('data-settings-inline')) continue;
      section.dataset.dialogReady = 'true';
      const heading = section.querySelector('h2');
      const title = heading.textContent;
      const summary = document.createElement('p');
      summary.className = 'muted settings-summary';
      summary.textContent = summaries[section.id]?.(section) || 'Manage this setting';
      const button = document.createElement('button');
      button.type = 'button'; button.className = 'btn-secondary settings-edit'; button.textContent = 'Edit';
      button.setAttribute('aria-label', `Edit ${title.toLowerCase()}`);
      const dialog = document.createElement('dialog');
      dialog.className = 'settings-dialog';
      const label = document.createElement('h2');
      label.id = `${section.id}-dialog-title`; label.textContent = title;
      dialog.setAttribute('aria-labelledby', label.id);
      dialog.append(label);
      for (const node of Array.from(section.childNodes)) if (node !== heading) dialog.append(node);
      const close = document.createElement('button');
      close.type = 'button'; close.className = 'btn-secondary dialog-close'; close.textContent = 'Close';
      dialog.append(close);
      section.append(summary, button, dialog);
      button.addEventListener('click', () => { active = section.id; dialog.showModal(); });
      close.addEventListener('click', () => dialog.close());
      dialog.addEventListener('close', () => {
        for (const form of dialog.querySelectorAll('form')) form.reset();
        if (section.isConnected) { active = null; button.focus(); }
      });
    }
  }
  enhance();
  const initial = root.dataset.activeSection;
  if (initial && root.querySelector(`#${CSS.escape(initial)} .error`)) {
    active = initial; root.querySelector(`#${CSS.escape(initial)} dialog`)?.showModal();
  }
  document.addEventListener('fx:before', () => {
    const section = active && document.getElementById(active);
    submitted = section && active !== 'key-storage' ? Array.from(section.querySelectorAll('input, select, textarea')).map(input => ({
      name: input.name, value: input.value, checked: input.checked, type: input.type
    })) : null;
  });
  document.addEventListener('fx:after', event => {
    const response = event.detail.cfg.response;
    const dialog = active && document.getElementById(active)?.querySelector('dialog');
    if (dialog && response && !response.ok && response.status !== 422) {
      let error = dialog.querySelector('.dialog-request-error');
      if (!error) { error = document.createElement('p'); error.className = 'error dialog-request-error'; error.setAttribute('role', 'alert'); dialog.prepend(error); }
      error.textContent = 'Could not save this setting. Please try again.';
    }
  });
  document.addEventListener('fx:swapped', () => {
    const previous = active;
    enhance();
    if (!previous) return;
    const section = document.getElementById(previous);
    const dialog = section?.querySelector('dialog');
    if (!dialog) return;
    // A webhook secret is shown once; keep its dialog open until dismissed.
    const secret = section.querySelector('[data-webhook-secret]');
    if (section.querySelector('.error') || secret) {
      if (submitted && section.querySelector('.error')) {
        const fields = Array.from(section.querySelectorAll('input, select, textarea'));
        submitted.forEach((saved, index) => {
          const input = fields[index];
          if (!input || input.name !== saved.name || input.type === 'hidden' || input.type === 'file') return;
          if (input.type === 'checkbox' || input.type === 'radio') input.checked = saved.checked;
          else input.value = saved.value;
        });
      }
      active = previous;
      if (!dialog.open) dialog.showModal();
      dialog.querySelector('[data-fx-focus]')?.focus();
    } else if (section.querySelector('[data-settings-saved]')) {
      if (dialog.open) dialog.close();
      active = null;
      const notice = section.querySelector('[data-settings-saved]');
      if (notice) section.insertBefore(notice, section.querySelector('dialog'));
      section.querySelector('.settings-edit')?.focus();
    }
  });
});
