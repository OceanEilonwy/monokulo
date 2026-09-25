/* Progressive checkout updates and local refund-QR capture. The HTML and
 * refund form remain usable when this script cannot run - a noscript
 * meta-refresh keeps the page current instead. */
(function () {
  'use strict';
  var root = document.getElementById('checkout-root');
  if (!root) return;
  var paymentAddress = document.getElementById('address');
  var copyAddress = document.getElementById('copy-address');
  if (paymentAddress) paymentAddress.addEventListener('dblclick', function () { paymentAddress.select(); });
  if (paymentAddress && copyAddress) {
    copyAddress.hidden = false;
    copyAddress.addEventListener('click', async function () {
      try {
        if (!navigator.clipboard || !navigator.clipboard.writeText) throw new Error('Clipboard unavailable');
        await navigator.clipboard.writeText(paymentAddress.value);
      } catch (_) {
        paymentAddress.select();
        if (typeof document.execCommand !== 'function' || !document.execCommand('copy')) {
          copyAddress.setAttribute('aria-label', 'Could not copy; address selected');
          return;
        }
      }
      copyAddress.setAttribute('aria-label', 'Payment address copied');
      setTimeout(function () { copyAddress.setAttribute('aria-label', 'Copy payment address'); }, 2000);
    });
  }
  var refundInput = document.getElementById('refund_address');
  var refundForm = document.getElementById('refund-form');
  var refundField = document.getElementById('refund-field');
  var saveState = document.getElementById('refund-save-state');
  var scanButton = document.getElementById('scan-refund');
  var uploadButton = document.getElementById('upload-refund');
  var fileInput = document.getElementById('refund-image');
  var video = document.getElementById('refund-camera');
  var scanError = document.getElementById('scan-error');
  var stream = null;
  var scanning = false;
  var starting = false;
  var frameTimer = null;
  var canvas = document.createElement('canvas');
  var context = canvas.getContext('2d', { willReadFrequently: true });
  var savedAddress = refundField && refundField.classList.contains('is-saved') ? refundInput.value.trim() : '';
  var saveTimer = null;
  var saving = false;

  function validAddress(value) {
    return /^(?:[1-9A-HJ-NP-Za-km-z]{95}|[1-9A-HJ-NP-Za-km-z]{106})$/.test(value);
  }

  function saveDisplay(state) {
    if (!refundField) return;
    refundField.classList.toggle('is-saving', state === 'saving');
    refundField.classList.toggle('is-saved', state === 'saved');
    refundField.classList.toggle('is-invalid', state === 'invalid');
    refundInput.setAttribute('aria-invalid', state === 'invalid' ? 'true' : 'false');
    saveState.setAttribute('aria-label', state === 'saving' ? 'Saving refund address' : state === 'saved' ? 'Refund address saved' : state === 'invalid' ? 'Invalid refund address' : '');
  }

  function scheduleSave(delay) {
    if (!refundInput || !refundForm) return;
    clearTimeout(saveTimer);
    var value = refundInput.value.trim();
    if (!validAddress(value) || value === savedAddress || saving) return;
    saveTimer = setTimeout(saveAddress, delay);
  }

  async function saveAddress() {
    var value = refundInput.value.trim();
    if (saving || !validAddress(value) || value === savedAddress) return;
    saving = true;
    saveDisplay('saving');
    error('');
    try {
      var response = await fetch(refundForm.action, {
        method: 'POST', headers: { 'Accept': 'application/json', 'Content-Type': 'application/x-www-form-urlencoded' },
        body: new URLSearchParams({ refund_address: value }).toString()
      });
      var result = await response.json();
      if (!response.ok || !result.ok) {
        var failure = new Error(result.error || 'Could not save the refund address. Try again.');
        failure.invalid = response.status === 400;
        throw failure;
      }
      savedAddress = value;
      if (refundInput.value.trim() === value) saveDisplay('saved');
      else saveDisplay('');
    } catch (problem) {
      if (refundInput.value.trim() === value) {
        saveDisplay(problem.invalid ? 'invalid' : '');
        error(problem.invalid ? 'Invalid refund address. Enter a valid address for this store’s network.' : problem.message || 'Could not save the refund address. Try again.');
      } else saveDisplay('');
    } finally {
      saving = false;
      if (refundInput.value.trim() !== value) scheduleSave(0);
    }
  }

  if (refundInput) {
    refundInput.addEventListener('focus', function () { if (refundField.classList.contains('is-saved')) saveDisplay(''); });
    refundInput.addEventListener('pointerdown', function () { if (refundField.classList.contains('is-saved')) saveDisplay(''); });
    refundInput.addEventListener('input', function () {
      saveDisplay(saving ? 'saving' : '');
      error('');
      scheduleSave(450);
    });
    refundInput.addEventListener('blur', function () {
      var value = refundInput.value.trim();
      if (!value && savedAddress) {
        refundInput.value = savedAddress;
        saveDisplay('saved');
      } else if (value && !validAddress(value)) error('Enter a Monero address with 95 or 106 valid characters.');
      else if (value && value === savedAddress) saveDisplay('saved');
    });
    refundForm.addEventListener('submit', function (event) { event.preventDefault(); scheduleSave(0); });
  }

  function error(message) {
    if (!scanError) return;
    scanError.textContent = message;
    scanError.hidden = !message;
  }

  function stopCamera() {
    scanning = false;
    if (frameTimer) clearTimeout(frameTimer);
    frameTimer = null;
    if (stream) stream.getTracks().forEach(function (track) { track.stop(); });
    stream = null;
    if (video) { video.srcObject = null; video.hidden = true; }
    if (scanButton) { scanButton.setAttribute('aria-label', 'Scan refund QR'); scanButton.title = 'Scan refund QR'; }
  }

  function addressFromQr(value) {
    var data = value.trim();
    var match = /^monero:([^?]+)/i.exec(data);
    if (match) data = decodeURIComponent(match[1]);
    if (!validAddress(data)) return null;
    return data;
  }

  function useResult(value) {
    var address;
    try { address = addressFromQr(value); } catch (_) { address = null; }
    if (!address) {
      error('This QR code does not contain a Monero address.');
      return false;
    }
    refundInput.value = address;
    refundInput.focus();
    error('');
    stopCamera();
    scheduleSave(0);
    return true;
  }

  function decode(width, height) {
    if (!context || typeof window.jsQR !== 'function') return false;
    var image = context.getImageData(0, 0, width, height);
    var result = window.jsQR(image.data, width, height, { inversionAttempts: 'attemptBoth' });
    return result ? useResult(result.data) : false;
  }

  function drawImage(source, width, height) {
    var scale = Math.min(1, 1600 / Math.max(width, height));
    canvas.width = Math.max(1, Math.round(width * scale));
    canvas.height = Math.max(1, Math.round(height * scale));
    context.drawImage(source, 0, 0, canvas.width, canvas.height);
  }

  function scanFrame() {
    if (!scanning || !video) return;
    if (video.readyState >= 2 && video.videoWidth > 0) {
      canvas.width = Math.min(video.videoWidth, 640);
      canvas.height = Math.round(video.videoHeight * canvas.width / video.videoWidth);
      context.drawImage(video, 0, 0);
      if (decode(canvas.width, canvas.height)) return;
    }
    frameTimer = setTimeout(scanFrame, 200);
  }

  if (refundInput && context && typeof window.jsQR === 'function') {
    uploadButton.hidden = false;
    uploadButton.addEventListener('click', function () { fileInput.click(); });
    fileInput.addEventListener('change', async function () {
      var file = fileInput.files && fileInput.files[0];
      if (!file) return;
      stopCamera();
      error('');
      try {
        if (typeof createImageBitmap === 'function') {
          var image = await createImageBitmap(file);
          drawImage(image, image.width, image.height);
          image.close();
        } else {
          var url = URL.createObjectURL(file);
          try {
            var picture = new Image();
            picture.src = url;
            await new Promise(function (resolve, reject) { picture.onload = resolve; picture.onerror = reject; });
            drawImage(picture, picture.naturalWidth, picture.naturalHeight);
          } finally { URL.revokeObjectURL(url); }
        }
        if (!decode(canvas.width, canvas.height)) error('No QR code found in that image.');
      } catch (_) { error('Could not read that image. Choose another file.'); }
      fileInput.value = '';
    });
    if (navigator.mediaDevices && navigator.mediaDevices.getUserMedia) {
      scanButton.hidden = false;
      scanButton.addEventListener('click', async function () {
        if (scanning) { stopCamera(); return; }
        // getUserMedia can take a while (permission prompt, slow device); a
        // second click in that window would open a second, orphaned stream.
        if (starting) return;
        starting = true;
        // Clear any previous error only once the camera is running: clearing it
        // up front collapses the message, then a repeat failure re-shows it and
        // the layout jumps on every click.
        try {
          stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' }, audio: false });
          video.srcObject = stream;
          video.hidden = false;
          await video.play();
          error('');
          scanning = true;
          scanButton.setAttribute('aria-label', 'Stop camera');
          scanButton.title = 'Stop camera';
          scanFrame();
        } catch (_) {
          stopCamera();
          error('Camera unavailable. Choose a QR image instead.');
        } finally {
          starting = false;
        }
      });
    }
  }
  window.addEventListener('pagehide', stopCamera);

  // Live updates: the server streams re-rendered copies of the page's
  // changing parts (`[data-live]` elements) whenever the order changes, and
  // each is swapped in by id. Nothing else is touched, so a refund address
  // mid-edit or a camera scan in progress carries on undisturbed. The
  // stream ends once the order is final.
  var initialStatus = root.dataset.status;
  if (initialStatus === 'paid' || initialStatus === 'overpaid' || initialStatus === 'expired') return;
  if (typeof EventSource !== 'function') {
    setTimeout(function () { location.reload(); }, 60000);
    return;
  }
  var params = new URLSearchParams(location.search);
  params.set('fragments', 'true');
  var updates = new EventSource(location.pathname + '/events?' + params.toString());
  updates.addEventListener('fragment', function (event) {
    var template = document.createElement('template');
    template.innerHTML = event.data;
    Array.prototype.forEach.call(template.content.querySelectorAll('[data-live][id]'), function (fresh) {
      var current = document.getElementById(fresh.id);
      if (current && current.outerHTML !== fresh.outerHTML) current.replaceWith(fresh);
    });
  });
  updates.addEventListener('status', function (event) {
    var state;
    try { state = JSON.parse(event.data); } catch (_) { return; }
    root.dataset.status = state.status;
    if (state.is_terminal) updates.close();
  });
  window.addEventListener('pagehide', function () { updates.close(); });
  // Restored from the back/forward cache with the stream closed.
  window.addEventListener('pageshow', function (event) { if (event.persisted) location.reload(); });
})();
