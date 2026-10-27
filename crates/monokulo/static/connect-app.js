// Connect an app (views/connected_apps.rs): the device name, as it's typed,
// in the preview of how the app will read under Apps. Without this script
// the preview shows the name the page was rendered with.
(function () {
  document.querySelectorAll('input[data-preview]').forEach(function (input) {
    var preview = document.getElementById(input.getAttribute('data-preview'));
    if (!preview) return;
    var name = preview.querySelector('span');
    input.addEventListener('input', function () {
      var value = input.value.trim();
      name.textContent = value;
      preview.hidden = value === '';
    });
  });
})();
