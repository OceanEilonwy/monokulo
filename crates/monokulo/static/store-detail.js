// The store page: the POS link is a link only with JavaScript, which the
// POS needs; without it the widget says so.
(function () {
  var link = document.getElementById('pos-launch');
  if (!link) return;
  link.href = link.dataset.href;
  link.removeAttribute('aria-disabled');
  link.removeAttribute('tabindex');
  link.classList.remove('pos-launch-disabled');
  document.getElementById('pos-launch-hint').textContent = 'Full-screen keypad for in-person sales';
})();
