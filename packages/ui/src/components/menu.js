// Native details owns activation. Enhancement adds dismissal and focus return.
document.addEventListener('click', event => {
  document.querySelectorAll('details.mk-menu[open]').forEach(menu => {
    if (!menu.contains(event.target)) menu.open = false;
  });
});
document.addEventListener('keydown', event => {
  if (event.key !== 'Escape') return;
  const menu = event.target.closest('details.mk-menu[open]');
  if (menu) { menu.open = false; menu.querySelector('summary').focus(); event.preventDefault(); }
});
