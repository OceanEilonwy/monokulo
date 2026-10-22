/* Progressive checkout updates and local refund-QR capture. The HTML and
 * refund form remain usable when this script cannot run - a noscript
 * meta-refresh keeps the page current instead. */
(function () {
  'use strict';
  var root = document.getElementById('checkout-root');
  if (!root) return;
  // Times come in UTC unless the page was given ?timezone=; those marked
  // data-local are shown in the customer's own zone instead, in the same
  // shape the server uses ("14 Nov, 22:13", with the year when it isn't
  // this one). Run again for each part the live stream swaps in.
  var MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];
  function showLocalTimes() {
    if (typeof Intl !== 'object' || !Intl.DateTimeFormat) return;
    var zone;
    try { zone = Intl.DateTimeFormat().resolvedOptions().timeZone; } catch (_) { return; }
    if (!zone) return;
    var now = new Date();
    var times = document.querySelectorAll('time[data-local]');
    for (var i = 0; i < times.length; i++) {
      var at = new Date(times[i].getAttribute('datetime'));
      if (isNaN(at.getTime())) continue;
      // Numbers from Intl, month names our own: locales disagree on them
      // ("Sep" or "Sept"), and this must match what the server writes.
      var parts = {};
      new Intl.DateTimeFormat('en-GB', { year: 'numeric', month: 'numeric', day: 'numeric', hour: '2-digit', minute: '2-digit', hourCycle: 'h23' })
        .formatToParts(at).forEach(function (part) { parts[part.type] = part.value; });
      var sameYear = Number(parts.year) === Number(new Intl.DateTimeFormat('en-GB', { year: 'numeric' }).format(now));
      times[i].textContent = Number(parts.day) + ' ' + MONTHS[Number(parts.month) - 1] + (sameYear ? '' : ' ' + parts.year) + ', ' + parts.hour + ':' + parts.minute;
      times[i].title = new Intl.DateTimeFormat('en-GB', { dateStyle: 'full', timeStyle: 'medium' }).format(at) + ' (' + zone + ')';
      times[i].removeAttribute('data-local');
    }
  }
  showLocalTimes();
  document.addEventListener('fx:sse:swapped', showLocalTimes);

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

  // The save itself goes through fixi (the form's fx-action, triggered by
  // a "refund:save" event), answered in JSON; this script keeps the
  // debounce and what the field shows.
  var pendingValue = null;

  function saveAddress() {
    var value = refundInput.value.trim();
    if (saving || !validAddress(value) || value === savedAddress) return;
    saving = true;
    pendingValue = value;
    saveDisplay('saving');
    error('');
    refundForm.dispatchEvent(new CustomEvent('refund:save'));
  }

  function saveFinished(problem) {
    var value = pendingValue;
    saving = false;
    pendingValue = null;
    if (!problem) {
      savedAddress = value;
      if (refundInput.value.trim() === value) saveDisplay('saved');
      else saveDisplay('');
    } else if (refundInput.value.trim() === value) {
      saveDisplay(problem.invalid ? 'invalid' : '');
      error(problem.invalid ? 'Invalid refund address. Enter a valid address for this store’s network.' : problem.message || 'Could not save the refund address. Try again.');
    } else saveDisplay('');
    if (refundInput.value.trim() !== value) scheduleSave(0);
  }

  if (refundForm) {
    refundForm.addEventListener('fx:config', function (event) {
      if (!saving) { event.preventDefault(); return; }
      event.detail.cfg.headers.Accept = 'application/json';
      event.detail.cfg.body = new URLSearchParams({ refund_address: pendingValue });
    });
    refundForm.addEventListener('fx:after', function (event) {
      var response = event.detail.cfg.response, result = {};
      try { result = JSON.parse(event.detail.cfg.text); } catch (_) {}
      if (response.ok && result.ok) { saveFinished(null); return; }
      var failure = new Error(result.error || 'Could not save the refund address. Try again.');
      failure.invalid = response.status === 400;
      saveFinished(failure);
    });
    refundForm.addEventListener('fx:error', function (event) {
      var problem = event.detail && event.detail.error;
      saveFinished(new Error((problem && problem.message) || 'Could not save the refund address. Try again.'));
    });
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

  // The QR decoder is fetched the first time a customer scans or picks an
  // image, not with the page: most never do. The form says where it is.
  var decoderUrl = refundForm && refundForm.dataset.qrDecoder;
  var decoderLoading = null;
  function loadDecoder() {
    if (typeof window.jsQR === 'function') return Promise.resolve();
    if (!decoderLoading) {
      decoderLoading = new Promise(function (resolve, reject) {
        var script = document.createElement('script');
        script.src = decoderUrl;
        script.onload = function () { typeof window.jsQR === 'function' ? resolve() : reject(new Error('QR decoder unavailable')); };
        script.onerror = function () { decoderLoading = null; reject(new Error('QR decoder unavailable')); };
        document.head.appendChild(script);
      });
    }
    return decoderLoading;
  }

  // true: a Monero address was used; false: a QR code without one (already
  // explained); null: no QR code found.
  function decode(width, height) {
    if (!context || typeof window.jsQR !== 'function') return null;
    var image = context.getImageData(0, 0, width, height);
    var result = window.jsQR(image.data, width, height, { inversionAttempts: 'attemptBoth' });
    return result ? useResult(result.data) : null;
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

  if (refundInput && context && decoderUrl) {
    uploadButton.hidden = false;
    uploadButton.addEventListener('click', function () {
      // Fetched while the customer picks the file.
      loadDecoder().catch(function () {});
      fileInput.click();
    });
    fileInput.addEventListener('change', async function () {
      var file = fileInput.files && fileInput.files[0];
      if (!file) return;
      stopCamera();
      error('');
      try {
        await loadDecoder();
      } catch (_) {
        error('Could not load the QR reader. Try again.');
        fileInput.value = '';
        return;
      }
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
        if (decode(canvas.width, canvas.height) === null) error('No QR code found in that image.');
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
          // The decoder comes while the camera starts.
          var decoder = loadDecoder();
          decoder.catch(function () {});
          stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: 'environment' }, audio: false });
          await decoder;
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

  // Live updates (structured_logging.md part 8): fixi opens the order's
  // event stream from #checkout-stream as soon as it sees it, and ssexi
  // routes each changed part of the page (the `[data-live]` elements) to
  // the element it replaces. Nothing else is touched, so a refund address
  // mid-edit or a camera scan in progress carries on undisturbed. A
  // dropped stream reconnects with Last-Event-ID (fx-sse-reconnect); the
  // server says "done" once the order is final. The page knows nothing
  // about whatever embeds it; an embedder follows the order on its own.
  var streamElement = document.getElementById('checkout-stream');
  if (!streamElement) return;
  if (typeof window.ReadableStream !== 'function' || typeof window.fetch !== 'function') {
    // No streaming fetch: fall back to reloading, as without JavaScript.
    setTimeout(function () { location.reload(); }, 60000);
    return;
  }
  var liveStream = null;
  // A refused stream (server error, restart, too many open streams) is
  // retried with backoff: 5s, doubling to at most a minute.
  var STREAM_RETRY_MS = 5000;
  var STREAM_RETRY_MAX_MS = 60000;
  var retryDelay = STREAM_RETRY_MS;
  var finished = false;
  streamElement.addEventListener('fx:sse:open', function (event) {
    liveStream = event.detail.cfg.sse;
    retryDelay = STREAM_RETRY_MS;
  });
  streamElement.addEventListener('fx:after', function (event) {
    var response = event.detail.cfg.response;
    if (finished || !response || response.ok) return;
    setTimeout(function () { streamElement.dispatchEvent(new CustomEvent('fx:inited')); }, retryDelay);
    retryDelay = Math.min(retryDelay * 2, STREAM_RETRY_MAX_MS);
  });
  document.addEventListener('fx:sse:status', function (event) {
    var state;
    try { state = JSON.parse(event.detail.message.data); } catch (_) { return; }
    root.dataset.status = state.status;
    if (state.is_terminal) finished = true;
  });
  window.addEventListener('pagehide', function () { finished = true; if (liveStream) liveStream.close(); liveStream = null; });
  // Restored from the back/forward cache with the stream closed.
  window.addEventListener('pageshow', function (event) { if (event.persisted) location.reload(); });
})();
