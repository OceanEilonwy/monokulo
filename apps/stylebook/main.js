import '@monokulo/ui/islands';
// A specimen's POST form (the theme switch, Log out) has nowhere to post on
// a static host: it stays on the page.
document.addEventListener('submit', event => {
  if (event.target.closest('.book-example') && event.target.method === 'post') event.preventDefault();
}, true);
// The stylebook's own theme switch: system, light or dark.
const theme = document.querySelector('#book-theme');
document.querySelector('.book-theme').hidden = false;
theme.addEventListener('change', () => {
  if (theme.value === 'system') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.dataset.theme = theme.value;
});
