// The wallet pages' script (docs/wallets.md).
//
// On "Set up your wallet" it turns on "Create a new wallet", which the
// server draws unavailable so a browser without JavaScript is told why.
//
// On "Create a new wallet" it makes the wallet with the wallet-setup
// WebAssembly module: 32 random bytes from crypto.getRandomValues become a
// 16-word polyseed, its watch-only keys and address. Every screen (back up,
// check, skip) is drawn by the server; this shows one at a time, fills in
// the words and QR codes, and at the end posts only the private view key,
// the public spend key, the address and how the phrase was backed up. The
// phrase never leaves the page.
(() => {
  'use strict';

  const supported =
    typeof WebAssembly === 'object' &&
    typeof window.crypto === 'object' &&
    typeof window.crypto.getRandomValues === 'function';

  const choice = document.querySelector('[data-wallet-choice]');
  if (choice) {
    const card = choice.querySelector('[data-needs-js]');
    const reason = choice.querySelector('[data-needs-js-reason]');
    if (supported && card) {
      card.classList.remove('unavailable');
      const tag = card.querySelector('[data-js-tag]');
      if (tag) tag.textContent = 'Recommended';
      const button = card.querySelector('[data-create-wallet]');
      if (button) button.disabled = false;
      if (reason) reason.hidden = true;
    } else if (reason) {
      reason.textContent =
        "This browser can't make a wallet: it has no WebAssembly or no secure random numbers. Bring your own wallet instead.";
    }
  }

  const root = document.querySelector('[data-wallet-setup]');
  if (!root) return;

  const $ = (selector, within = root) => within.querySelector(selector);
  const $$ = (selector, within = root) => Array.from(within.querySelectorAll(selector));
  const screens = $$('[data-screen]');
  const show = (name) => {
    for (const screen of screens) screen.hidden = screen.dataset.screen !== name;
    const shown = $(`[data-screen="${name}"]`);
    const heading = shown && shown.querySelector('h1, h2');
    if (heading) {
      heading.tabIndex = -1;
      heading.focus();
    }
    window.scrollTo(0, 0);
  };
  const fail = (why) => {
    $('[data-failed-reason]').textContent = why;
    show('failed');
  };
  if (!supported) {
    fail('It has no WebAssembly or no secure random numbers, which making a wallet needs.');
    return;
  }

  let wallet = null;
  let words = [];
  let legacyWords = [];
  let submitted = false;
  let exports = null;

  const call = (fn, bytes) => {
    const ptr = exports.alloc(bytes.length);
    new Uint8Array(exports.memory.buffer, ptr, bytes.length).set(bytes);
    let length;
    try {
      length = exports[fn](ptr, bytes.length);
    } finally {
      exports.dealloc(ptr, bytes.length);
      bytes.fill(0);
    }
    const out = new Uint8Array(exports.memory.buffer, exports.output_ptr(), length);
    const parsed = JSON.parse(new TextDecoder().decode(out));
    exports.clear_output();
    if (parsed.error) throw new Error(parsed.error);
    return parsed;
  };

  const hex = (bytes) => Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');

  const fillWords = (list, items) => {
    list.replaceChildren(
      ...items.map((word, i) => {
        const li = document.createElement('li');
        li.dataset.n = String(i + 1);
        li.textContent = word;
        return li;
      }),
    );
  };

  // -- Back up ----------------------------------------------------------------

  const method = () => ($('[data-method-choice]:checked') || {}).value || 'app';
  const currentApp = () =>
    ($('[data-app-tab][aria-selected="true"]') || {}).dataset?.appTab || 'cake';
  const backedUp = $('[data-backed-up]');
  const next = $('[data-go="check"]');

  const coverQrs = () => {
    for (const panel of $$('[data-app-panel]')) {
      const frame = $('[data-qr]', panel);
      if (!frame) continue;
      frame.replaceChildren();
      frame.hidden = true;
      $('[data-qr-cover]', panel).hidden = false;
      $('[data-qr-caption]', panel).hidden = true;
      $('[data-show-qr]', panel).hidden = false;
      $('[data-hide-qr]', panel).hidden = true;
    }
  };

  const qrText = (kind) => {
    if (kind === 'word-list') return JSON.stringify({ mnemonic: words });
    const height = root.dataset.restoreHeight;
    let link = `monero_wallet:${wallet.address}?seed=${encodeURIComponent(wallet.phrase)}`;
    if (height) link += `&height=${height}`;
    link += `&label=${encodeURIComponent(root.dataset.name)}`;
    return link;
  };

  const showQr = (panel) => {
    const frame = $('[data-qr]', panel);
    const { svg } = call('qr', new TextEncoder().encode(qrText(frame.dataset.qr)));
    // The module's own drawing of a QR code (no text from the page in it).
    frame.innerHTML = svg;
    const drawn = frame.querySelector('svg');
    if (drawn) {
      drawn.setAttribute('role', 'img');
      drawn.setAttribute('aria-label', 'QR code holding the recovery phrase');
    }
    frame.hidden = false;
    $('[data-qr-cover]', panel).hidden = true;
    $('[data-qr-caption]', panel).hidden = false;
    $('[data-show-qr]', panel).hidden = true;
    $('[data-hide-qr]', panel).hidden = false;
  };

  const resetBackedUp = () => {
    backedUp.checked = false;
    next.disabled = true;
  };

  const chooseMethod = (value) => {
    for (const radio of $$('[data-method-choice]')) radio.checked = radio.value === value;
    for (const panel of $$('[data-method-panel]')) panel.hidden = panel.dataset.methodPanel !== value;
    for (const label of $$('[data-backed-up-label]')) label.hidden = label.dataset.backedUpLabel !== value;
    coverQrs();
    resetBackedUp();
  };

  const chooseApp = (key) => {
    for (const tab of $$('[data-app-tab]')) {
      tab.setAttribute('aria-selected', tab.dataset.appTab === key ? 'true' : 'false');
    }
    for (const panel of $$('[data-app-panel]')) panel.hidden = panel.dataset.appPanel !== key;
    coverQrs();
    resetBackedUp();
  };

  for (const radio of $$('[data-method-choice]')) {
    radio.addEventListener('change', () => chooseMethod(radio.value));
  }
  for (const tab of $$('[data-app-tab]')) {
    tab.addEventListener('click', () => chooseApp(tab.dataset.appTab));
  }
  root.addEventListener('click', (event) => {
    const target = event.target.closest('button');
    if (!target || !root.contains(target)) return;
    const panel = target.closest('[data-app-panel]');
    if (target.matches('[data-show-qr]')) showQr(panel);
    else if (target.matches('[data-hide-qr]')) coverQrs();
    else if (target.matches('[data-show-paper]')) chooseMethod('paper');
    else if (target.matches('[data-show-legacy]')) {
      $('[data-legacy]', panel).hidden = false;
      target.hidden = true;
    } else if (target.matches('[data-toggle-words]')) {
      const list = $('[data-words]');
      const concealed = list.classList.toggle('concealed');
      target.textContent = concealed ? 'Show words' : 'Hide words';
    } else if (target.matches('[data-print]')) {
      window.print();
    } else if (target.matches('[data-go]')) {
      const to = target.dataset.go;
      if (to === 'check') startCheck();
      else if (to === 'skip') startSkip();
      else {
        coverQrs();
        show(to);
      }
    } else if (target.matches('[data-finish]')) finishCheck();
    else if (target.matches('[data-skip-confirm]')) submit('skipped');
  });
  backedUp.addEventListener('change', () => {
    next.disabled = !backedUp.checked;
  });

  // -- Check --------------------------------------------------------------------

  let backup = 'paper';
  let asked = [];

  const backupKey = () => (method() === 'paper' ? 'paper' : currentApp());

  const pick = (count, from) => {
    const chosen = new Set();
    const random = new Uint32Array(1);
    while (chosen.size < count) {
      crypto.getRandomValues(random);
      chosen.add(random[0] % from);
    }
    return Array.from(chosen).sort((a, b) => a - b);
  };

  const startCheck = () => {
    coverQrs();
    backup = backupKey();
    const source = backup === 'gui' ? legacyWords : words;
    asked = pick(3, source.length).map((index) => ({ index, word: source[index] }));
    for (const box of $$('[data-find]')) box.hidden = box.dataset.find !== backup;
    $$('[data-word-check]').forEach((label, i) => {
      label.classList.remove('ok', 'bad');
      const n = asked[i].index + 1;
      $('[data-word-label]', label).textContent =
        backup === 'gui' ? `Word ${n} of 25` : `Word ${n}`;
      const input = $('input', label);
      input.value = '';
      input.removeAttribute('aria-invalid');
      $('[data-word-message]', label).textContent = '';
      $('[data-word-message]', label).className = '';
    });
    show('check');
  };

  const checkOne = (label, i) => {
    const input = $('input', label);
    const typed = input.value.trim().toLowerCase();
    const message = $('[data-word-message]', label);
    const right = typed === asked[i].word;
    label.classList.toggle('ok', right);
    label.classList.toggle('bad', !right);
    input.setAttribute('aria-invalid', right ? 'false' : 'true');
    message.className = right ? 'field-ok' : 'field-error';
    message.textContent = right
      ? 'Matches.'
      : "That doesn't match. A wrong word in your backup means you can't open this wallet later.";
    return right;
  };

  for (const [i, label] of $$('[data-word-check]').entries()) {
    $('input', label).addEventListener('change', () => checkOne(label, i));
  }

  const finishCheck = () => {
    const results = $$('[data-word-check]').map((label, i) => checkOne(label, i));
    if (results.every(Boolean)) submit(backup);
    else {
      const first = $('[data-word-check].bad input');
      if (first) first.focus();
    }
  };

  // -- Skip ---------------------------------------------------------------------

  const understood = $('[data-skip-understood]');
  const typedSkip = $('[data-skip-typed]');
  const confirmSkip = $('[data-skip-confirm]');
  const updateSkip = () => {
    confirmSkip.disabled = !(understood.checked && typedSkip.value.trim().toLowerCase() === 'skip');
  };
  understood.addEventListener('change', updateSkip);
  typedSkip.addEventListener('input', updateSkip);
  const startSkip = () => {
    coverQrs();
    understood.checked = false;
    typedSkip.value = '';
    updateSkip();
    show('skip');
  };

  // -- Register -----------------------------------------------------------------

  const submit = (how) => {
    const form = $('[data-register]');
    $('[data-field="backup"]', form).value = how;
    $('[data-field="address"]', form).value = wallet.address;
    $('[data-field="view"]', form).value = wallet.view_key;
    $('[data-field="spend"]', form).value = wallet.spend_public_key;
    submitted = true;
    // The phrase isn't needed any more: drop every copy the page holds.
    for (const list of $$('[data-words], [data-legacy-words]')) list.replaceChildren();
    coverQrs();
    wallet.phrase = wallet.legacy_phrase = '';
    words = [];
    legacyWords = [];
    asked = [];
    // `requestSubmit` so key-custody.js can encrypt the keys first when
    // they go to SEV-SNP key storage.
    form.requestSubmit();
  };

  window.addEventListener('beforeunload', (event) => {
    if (wallet && !submitted) {
      event.preventDefault();
      event.returnValue = '';
    }
  });

  // -- Make the wallet ------------------------------------------------------------

  (async () => {
    try {
      const response = await fetch('/static/wallet-setup.wasm');
      if (!response.ok) throw new Error(`the wallet maker didn't load (${response.status})`);
      const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {});
      exports = instance.exports;
      const entropy = crypto.getRandomValues(new Uint8Array(32));
      const request = new TextEncoder().encode(
        JSON.stringify({
          entropy: hex(entropy),
          birthday: Math.floor(Date.now() / 1000),
          network: root.dataset.network,
        }),
      );
      entropy.fill(0);
      wallet = call('generate', request);
    } catch (error) {
      fail(`The wallet couldn't be made: ${error.message}`);
      return;
    }
    words = wallet.phrase.split(' ');
    legacyWords = wallet.legacy_phrase.split(' ');
    fillWords($('[data-words]'), words);
    for (const list of $$('[data-legacy-words]')) fillWords(list, legacyWords);
    chooseMethod('app');
    chooseApp('cake');
    show('backup');
  })();
})();
