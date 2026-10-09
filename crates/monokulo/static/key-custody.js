// Encrypts a store's keys in this browser for the engine's SEV-SNP key
// storage, so this site only ever relays them encrypted.
//
// A form with a key custody box (data-key-custody-bundle) is caught as it is
// submitted, before anything else sees it: key custody's own checker, built
// as WebAssembly (/static/key-custody.wasm), verifies the engine's bundle
// (AMD's signatures, the engine image, the key it vouches for) and encrypts
// the typed keys to it. The encrypted keys go in the form's encrypted_keys
// field, the typed ones are cleared, and the form is sent on as usual.
//
// Without this script the form still works: the merchant encrypts the keys
// with key-custody-cli and pastes the result (see views/key_entry.rs).
(() => {
  if (window.monokuloKeyCustody || !window.WebAssembly) return;
  window.monokuloKeyCustody = true;
  // Where the page says the module is (its URL carries the file's
  // version), read now: currentScript is only set while this runs.
  const MODULE_URL = (document.currentScript && document.currentScript.dataset.module) || '/static/key-custody.wasm';

  let loading;
  // The module, instantiated once. Its only import is randomness.
  const load = () => {
    loading ??= (async () => {
      let memory;
      const imports = {
        env: {
          fill_random: (ptr, len) => {
            crypto.getRandomValues(new Uint8Array(memory.buffer, ptr, len));
          },
        },
      };
      const response = await fetch(MODULE_URL);
      if (!response.ok) throw new Error(`the key checker didn't load (${response.status})`);
      const { instance } = await WebAssembly.instantiate(await response.arrayBuffer(), imports);
      memory = instance.exports.memory;
      return instance.exports;
    })();
    return loading;
  };

  // One call of the module's `seal`: JSON in, JSON out, through its memory.
  const seal = (exports, input) => {
    const bytes = new TextEncoder().encode(JSON.stringify(input));
    const ptr = exports.alloc(bytes.length);
    new Uint8Array(exports.memory.buffer, ptr, bytes.length).set(bytes);
    let length;
    try {
      length = exports.seal(ptr, bytes.length);
    } finally {
      // Wiped and freed: it held the keys.
      exports.dealloc(ptr, bytes.length);
      bytes.fill(0);
    }
    const out = new Uint8Array(exports.memory.buffer, exports.output_ptr(), length);
    return JSON.parse(new TextDecoder().decode(out));
  };

  const say = (box, text) => {
    const status = box.querySelector('[data-key-custody-status]');
    if (!status) return;
    status.textContent = text;
    status.hidden = false;
  };

  document.addEventListener(
    'submit',
    async (event) => {
      const form = event.target;
      if (!(form instanceof HTMLFormElement)) return;
      const box = form.querySelector('[data-key-custody-bundle]');
      if (!box) return;
      if (form.dataset.keyCustodySealed === 'yes') {
        delete form.dataset.keyCustodySealed;
        return;
      }
      const choice = box.dataset.keyCustodyBackendField;
      const backend = choice ? form.elements[choice] : null;
      if (backend && backend.value !== 'snp') return;
      const view = form.querySelector('[data-key-custody="view"]');
      const spend = form.querySelector('[data-key-custody="spend"]');
      const pasted = form.elements.encrypted_keys;
      if (!view || !spend || !pasted) return;
      // Encrypted with key-custody-cli and pasted in: nothing to do here.
      if (!view.value.trim() && pasted.value.trim()) return;

      event.preventDefault();
      event.stopImmediatePropagation();
      try {
        const out = seal(await load(), {
          bundle: JSON.parse(box.dataset.keyCustodyBundle),
          view_key: view.value.trim(),
          spend_public_key: spend.value.trim(),
          // Absent for the official key, which the checker has built in.
          id_key_digest: box.dataset.keyCustodyIdKey ?? null,
          min_guest_svn: Number(box.dataset.keyCustodyMinSvn || 0),
          min_tcb: box.dataset.keyCustodyMinTcb || '',
          now: Math.floor(Date.now() / 1000),
        });
        if (out.error) {
          say(box, `Your keys were not sent: ${out.error}`);
          return;
        }
        pasted.value = out.envelope;
        view.value = '';
        spend.value = '';
        say(
          box,
          `Encrypted for the engine (image ${out.measurement.slice(0, 16)}…, security version ${out.guest_svn}).`,
        );
        form.dataset.keyCustodySealed = 'yes';
        form.requestSubmit(event.submitter ?? undefined);
      } catch (error) {
        say(
          box,
          `This browser couldn't encrypt the keys (${error.message}), so they were not sent. Use key-custody-cli below.`,
        );
      }
    },
    true,
  );
})();
