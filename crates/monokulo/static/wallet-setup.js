// The wallet pages' script (docs/wallets.md).
//
// On the choice screen it turns on "Create a new wallet", which the server
// draws unavailable so a browser without JavaScript is told why.
//
// On "Create a new wallet" it makes the wallet with the wallet-setup
// WebAssembly module: 32 random bytes from crypto.getRandomValues become a
// 16-word polyseed, its watch-only keys and address. Every screen (back up,
// check, skip) is drawn by the server; this shows one at a time, fills in
// the words and QR codes straight away, makes a different phrase when asked,
// runs the two-word check, and at the end posts only the private view key,
// the public spend key, the address and how the phrase was backed up. The
// phrase never leaves the page, and nothing is posted before the check
// passes or is skipped, so leaving the page loses nothing that was made.
(() => {
  'use strict';
  // Where the page says the module is (its URL carries the file's
  // version), read now: currentScript is only set while this runs.
  const MODULE_URL = (document.currentScript && document.currentScript.dataset.module) || '/static/wallet-setup.wasm';
  // How long the check waits before it lets you go on without answering.
  const CHECK_SECONDS = 20;

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

  // A new wallet from fresh randomness.
  const generate = () => {
    const entropy = crypto.getRandomValues(new Uint8Array(32));
    const request = new TextEncoder().encode(
      JSON.stringify({
        entropy: hex(entropy),
        birthday: Math.floor(Date.now() / 1000),
        network: root.dataset.network,
      }),
    );
    entropy.fill(0);
    return call('generate', request);
  };

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

  const currentApp = () =>
    ($('[data-app-tab][aria-selected="true"]') || {}).dataset?.appTab || 'cake';
  const currentPanel = () => $(`[data-app-panel="${currentApp()}"]`);
  const next = $('[data-go="check"]');

  const updateNext = () => {
    const box = $('[data-backed-up]', currentPanel());
    next.disabled = !(box && box.checked);
  };

  const qrText = (kind) => {
    if (kind === 'word-list') return JSON.stringify({ mnemonic: words });
    const height = root.dataset.restoreHeight;
    let link = `monero_wallet:${wallet.address}?seed=${encodeURIComponent(wallet.phrase)}`;
    if (height) link += `&height=${height}`;
    link += `&label=${encodeURIComponent(root.dataset.name)}`;
    return link;
  };

  const drawQrs = () => {
    for (const frame of $$('[data-qr]')) {
      const { svg } = call('qr', new TextEncoder().encode(qrText(frame.dataset.qr)));
      // The module's own drawing of a QR code (no text from the page in it).
      frame.innerHTML = svg;
      const drawn = frame.querySelector('svg');
      if (drawn) {
        drawn.setAttribute('role', 'img');
        drawn.setAttribute('aria-label', 'QR code holding the recovery phrase');
      }
    }
  };

  // Shows `made` as the wallet: its words on every tab, its QR codes, and
  // nothing ticked yet.
  const useWallet = (made) => {
    wallet = made;
    words = wallet.phrase.split(' ');
    legacyWords = wallet.legacy_phrase.split(' ');
    for (const list of $$('[data-words]')) fillWords(list, words);
    for (const list of $$('[data-legacy-words]')) fillWords(list, legacyWords);
    drawQrs();
    for (const box of $$('[data-backed-up]')) box.checked = false;
    updateNext();
  };

  const chooseApp = (key) => {
    for (const tab of $$('[data-app-tab]')) {
      tab.setAttribute('aria-selected', tab.dataset.appTab === key ? 'true' : 'false');
    }
    for (const panel of $$('[data-app-panel]')) panel.hidden = panel.dataset.appPanel !== key;
    updateNext();
  };

  for (const tab of $$('[data-app-tab]')) {
    tab.addEventListener('click', () => chooseApp(tab.dataset.appTab));
  }
  for (const box of $$('[data-backed-up]')) box.addEventListener('change', updateNext);

  root.addEventListener('click', (event) => {
    const target = event.target.closest('button');
    if (!target || !root.contains(target)) return;
    if (target.matches('[data-regenerate]')) {
      try {
        useWallet(generate());
      } catch (error) {
        fail(`The wallet couldn't be made: ${error.message}`);
      }
    } else if (target.matches('[data-print]')) {
      window.print();
    } else if (target.matches('[data-go]')) {
      const to = target.dataset.go;
      if (to === 'check') startCheck();
      else if (to === 'skip') startSkip();
      else {
        stopCountdown();
        show(to);
      }
    } else if (target.matches('[data-pick]')) {
      pickWord(target);
    } else if (target.matches('[data-check-next]')) {
      stopCountdown();
      submit(backup);
    } else if (target.matches('[data-skip-confirm]')) {
      submit('skipped');
    }
  });

  // -- Check: two words, each picked from four --------------------------------

  let backup = 'cake';
  let asked = [];
  let countdown = null;
  let secondsLeft = CHECK_SECONDS;
  const checkNext = $('[data-check-next]');

  const randomBelow = (n) => {
    const random = new Uint32Array(1);
    crypto.getRandomValues(random);
    return random[0] % n;
  };

  const pick = (count, from) => {
    const chosen = new Set();
    while (chosen.size < count) chosen.add(randomBelow(from));
    return Array.from(chosen).sort((a, b) => a - b);
  };

  const shuffle = (items) => {
    for (let i = items.length - 1; i > 0; i--) {
      const j = randomBelow(i + 1);
      [items[i], items[j]] = [items[j], items[i]];
    }
    return items;
  };

  // Words from the same word list that aren't in the phrase: the words of
  // other wallets made just for this and thrown away.
  const decoyPool = (legacy, phrase) => {
    const pool = new Set();
    for (let tries = 0; tries < 8 && pool.size < 12; tries++) {
      const other = generate();
      const otherWords = (legacy ? other.legacy_phrase : other.phrase).split(' ');
      other.phrase = other.legacy_phrase = '';
      for (const word of otherWords) if (!phrase.includes(word)) pool.add(word);
    }
    return Array.from(pool);
  };

  const bothRight = () => asked.length > 0 && asked.every((q) => q.right);

  const updateCheckNext = () => {
    if (bothRight()) {
      stopCountdown();
      checkNext.disabled = false;
      checkNext.textContent = 'Next: add the wallet';
    } else if (secondsLeft <= 0) {
      checkNext.disabled = false;
      checkNext.textContent = 'Continue without checking';
    } else {
      checkNext.disabled = true;
      checkNext.replaceChildren(
        'Continue anyway in ',
        Object.assign(document.createElement('span'), {
          className: 'countdown',
          textContent: `${secondsLeft} s`,
        }),
      );
    }
  };

  const stopCountdown = () => {
    if (countdown) clearInterval(countdown);
    countdown = null;
  };

  const startCheck = () => {
    backup = currentApp();
    const legacy = backup === 'gui';
    const source = legacy ? legacyWords : words;
    const pool = decoyPool(legacy, source);
    asked = pick(2, source.length).map((index) => ({ index, word: source[index], right: false }));
    $$('[data-question]').forEach((box, i) => {
      const q = asked[i];
      const decoys = shuffle(pool.slice()).filter((w) => w !== q.word).slice(0, 3);
      const options = shuffle([q.word, ...decoys]);
      $('[data-question-label]', box).textContent = `Word ${q.index + 1}`;
      $$('[data-pick]', box).forEach((button, j) => {
        button.textContent = options[j];
        button.className = '';
        button.disabled = false;
        button.setAttribute('aria-pressed', 'false');
      });
      const note = $('[data-question-note]', box);
      note.textContent = '';
      note.className = 'q-note';
    });
    secondsLeft = CHECK_SECONDS;
    stopCountdown();
    countdown = setInterval(() => {
      secondsLeft -= 1;
      if (secondsLeft <= 0) stopCountdown();
      updateCheckNext();
    }, 1000);
    updateCheckNext();
    show('check');
  };

  const pickWord = (button) => {
    const box = button.closest('[data-question]');
    const q = asked[Number(box.dataset.question)];
    if (!q || q.right) return;
    const note = $('[data-question-note]', box);
    for (const other of $$('[data-pick]', box)) other.classList.remove('is-wrong');
    button.setAttribute('aria-pressed', 'true');
    if (button.textContent === q.word) {
      q.right = true;
      button.classList.add('is-right');
      for (const other of $$('[data-pick]', box)) if (other !== button) other.disabled = true;
      note.className = 'q-note ok';
      note.textContent = '✓ Right';
    } else {
      button.classList.add('is-wrong');
      note.className = 'q-note bad';
      note.textContent = `Not that one. Look at word ${q.index + 1} again.`;
    }
    updateCheckNext();
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
    stopCountdown();
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
    // The phrase isn't needed any more: drop every copy the page holds.
    for (const list of $$('[data-words], [data-legacy-words], [data-qr]')) list.replaceChildren();
    for (const button of $$('[data-pick]')) button.textContent = '';
    wallet.phrase = wallet.legacy_phrase = '';
    words = [];
    legacyWords = [];
    asked = [];
    // `requestSubmit` so key-custody.js can encrypt the keys first when
    // they go to SEV-SNP key storage.
    form.requestSubmit();
  };

  // -- Make the wallet ------------------------------------------------------------

  (async () => {
    try {
      const response = await fetch(MODULE_URL);
      if (!response.ok) throw new Error(`the wallet maker didn't load (${response.status})`);
      const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {});
      exports = instance.exports;
      useWallet(generate());
    } catch (error) {
      fail(`The wallet couldn't be made: ${error.message}`);
      return;
    }
    chooseApp('cake');
    show('backup');
  })();
})();
