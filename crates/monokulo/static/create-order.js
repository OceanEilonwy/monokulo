// The order form: a page shown again from the back/forward cache, or by
// Back, gets a fresh request key, so resubmitting it makes a new order
// rather than repeating the one it already made.
window.addEventListener('pageshow', function (event) {
  var navigation = performance.getEntriesByType('navigation')[0];
  if (event.persisted || (navigation && navigation.type === 'back_forward')) {
    var key = document.querySelector('input[name=request_key]');
    if (key) {
      if (crypto.randomUUID) key.value = crypto.randomUUID();
      else {
        var bytes = crypto.getRandomValues(new Uint8Array(16));
        key.value = Array.from(bytes, function (b) { return b.toString(16).padStart(2, '0'); }).join('');
      }
    }
  }
});
