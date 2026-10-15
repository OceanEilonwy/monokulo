import '@monokulo/ui/islands';
// The stylebook's own theme switch: system, light or dark.
const theme = document.querySelector('#book-theme');
document.querySelector('.book-theme').hidden = false;
theme.addEventListener('change', () => {
  if (theme.value === 'system') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.dataset.theme = theme.value;
});
