import '@monokulo/ui';
import '@monokulo/ui/islands';
const theme = document.querySelector('#book-theme');
document.querySelector('.book-theme').hidden = false;
theme.addEventListener('change', () => {
  if (theme.value === 'system') document.documentElement.removeAttribute('data-theme');
  else document.documentElement.dataset.theme = theme.value;
});
document.querySelector('#drive-select').hidden = false;
const form = document.querySelector('#demo-form');
form.addEventListener('submit', event => {
  event.preventDefault();
  document.querySelector('#demo-result').textContent = 'Example submitted. No account was created.';
});
document.querySelector('#drive-select').addEventListener('click', () => {
  const select = document.querySelector('#currency');
  select.value = select.value === 'AUD' ? 'EUR' : 'AUD';
  select.dispatchEvent(new Event('change', { bubbles: true }));
});
document.querySelector('#currency').addEventListener('change', event => {
  document.querySelector('#selected-currency').textContent = event.target.value;
});

const dialog = document.querySelector('#example-dialog');
document.querySelector('[data-open-dialog]').addEventListener('click', event => {
  if (typeof dialog.showModal !== 'function') return;
  event.preventDefault();
  dialog.showModal();
});
