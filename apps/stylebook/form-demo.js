import '@monokulo/ui/islands';

// Demo only: start from real defaults, then exercise normal change tracking.
if (new URLSearchParams(location.search).get('edits') === 'many') {
  setTimeout(() => {
    document.querySelectorAll('.setting-field input').forEach(input => {
      if (input.type === 'checkbox') input.checked = !input.checked;
      else input.value = input.value + ' (edited)';
      input.dispatchEvent(new Event('input', { bubbles: true }));
      input.dispatchEvent(new Event('change', { bubbles: true }));
    });
  }, 0);
}
