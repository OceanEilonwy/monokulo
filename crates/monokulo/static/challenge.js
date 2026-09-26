/* Solves the "Checking your connection" proof-of-work and continues by
 * itself (abuse protection, crate::abuse::challenge). The page works without
 * this script: a noscript meta refresh continues after a 10-second wait.
 *
 * The answer is a nonce such that SHA-256(challenge + nonce) starts with
 * `difficulty` zero bits. Hashes are computed with Web Crypto in batches so
 * the page stays responsive; where Web Crypto isn't available (a plain-HTTP
 * page outside a secure context) it falls back to the same 10-second wait
 * the no-JavaScript path uses. */
(function () {
  'use strict';
  var root = document.getElementById('challenge');
  if (!root) return;
  var challenge = root.getAttribute('data-challenge');
  var difficulty = parseInt(root.getAttribute('data-difficulty'), 10);
  var continueUrl = root.getAttribute('data-continue');
  var waitUrl = root.getAttribute('data-wait');
  var progress = document.getElementById('challenge-progress');
  progress.hidden = false;

  function say(text) { progress.textContent = text; }

  function leadingZeroBits(bytes) {
    var bits = 0;
    for (var i = 0; i < bytes.length; i++) {
      if (bytes[i] === 0) { bits += 8; continue; }
      var b = bytes[i];
      while ((b & 0x80) === 0) { bits++; b <<= 1; }
      return bits;
    }
    return bits;
  }

  function withParam(url, name, value) {
    return url + (url.indexOf('?') === -1 ? '?' : '&') + name + '=' + encodeURIComponent(value);
  }

  if (!window.crypto || !window.crypto.subtle || !window.TextEncoder) {
    say('Checking your connection, this page continues in 10 seconds.');
    setTimeout(function () { location.replace(waitUrl); }, 10500);
    return;
  }

  var encoder = new TextEncoder();
  var BATCH = 256;
  var nonce = 0;
  var started = Date.now();

  function batch() {
    var attempts = [];
    for (var i = 0; i < BATCH; i++) {
      attempts.push(String(nonce + i));
    }
    nonce += BATCH;
    return Promise.all(attempts.map(function (n) {
      return crypto.subtle.digest('SHA-256', encoder.encode(challenge + n)).then(function (hash) {
        return leadingZeroBits(new Uint8Array(hash)) >= difficulty ? n : null;
      });
    })).then(function (results) {
      for (var j = 0; j < results.length; j++) {
        if (results[j] !== null) return results[j];
      }
      return null;
    });
  }

  function step() {
    batch().then(function (found) {
      if (found !== null) {
        say('Done, continuing…');
        root.setAttribute('aria-busy', 'false');
        location.replace(withParam(continueUrl, 'monokulo_proof', challenge + '.' + found));
        return;
      }
      var seconds = Math.floor((Date.now() - started) / 1000);
      say(seconds < 2 ? 'Checking your connection…' : 'Checking your connection… (' + seconds + 's)');
      setTimeout(step, 0);
    }).catch(function () {
      say('Checking your connection, this page continues in 10 seconds.');
      setTimeout(function () { location.replace(waitUrl); }, 10500);
    });
  }
  say('Checking your connection…');
  step();
})();
