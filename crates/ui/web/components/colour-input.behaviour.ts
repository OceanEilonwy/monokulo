// The ColourInput component's behaviour (crates/ui/src/components/forms.rs):
// the swatch is what the form sends. The hex box beside it follows the swatch,
// and writes to it once it holds a whole colour, so a settings form sees any
// change on the swatch. Emptied, the box goes back to the default.
import { createSignal } from 'solid-js';
import type { ColourInputBehaviour, ColourInputProps } from '../../solid/ColourInput';

const NOT_A_COLOUR = 'Enter a colour like #2f6b4f.';

/** A whole colour as a colour input holds it, or undefined. */
function whole(text: string): string | undefined {
  const digits = text.trim().replace(/^#/, '').toLowerCase();
  return /^[0-9a-f]{6}$/.test(digits) ? `#${digits}` : undefined;
}

/** [`whole`], taking `#abc` too: only once typing is done, as it may go on. */
function finished(text: string): string | undefined {
  const digits = text.trim().replace(/^#/, '').toLowerCase();
  return /^[0-9a-f]{3}$/.test(digits) ? `#${digits.replace(/./g, '$&$&')}` : whole(digits);
}

export function setup(props: ColourInputProps): ColourInputBehaviour {
  // Set as the behaviour attaches, from the swatch's own value: `ownedWrite`.
  const [atDefault, setAtDefault] = createSignal(true, { ownedWrite: true });
  let well: HTMLInputElement | undefined;
  let hex: HTMLInputElement | undefined;

  const sync = () => {
    if (!well) return;
    setAtDefault(!props.default_value || well.value === props.default_value);
    // What's being typed stays as typed; anything else shows the colour.
    if (hex && document.activeElement !== hex) {
      hex.value = well.value;
      hex.setCustomValidity('');
    }
  };

  const write = (colour: string) => {
    if (!well) return;
    well.value = colour;
    well.dispatchEvent(new Event('input', { bubbles: true }));
    well.dispatchEvent(new Event('change', { bubbles: true }));
  };

  return {
    at_default: atDefault,
    reset() {
      if (props.default_value) write(props.default_value);
      well?.focus();
    },
    well(element) {
      well = element as HTMLInputElement;
      well.addEventListener('input', sync);
      well.addEventListener('change', sync);
      sync();
    },
    hex(element) {
      const box = element as HTMLInputElement;
      hex = box;
      box.addEventListener('input', () => {
        const colour = whole(box.value);
        box.setCustomValidity(colour || box.value.trim() === '' ? '' : NOT_A_COLOUR);
        if (colour && colour !== well?.value) write(colour);
      });
      box.addEventListener('blur', () => {
        const typed = box.value.trim();
        const colour = typed === '' ? props.default_value ?? well?.value : finished(typed);
        if (colour === undefined) {
          box.reportValidity();
          return;
        }
        if (colour !== well?.value) write(colour);
        box.value = colour;
        box.setCustomValidity('');
      });
      sync();
    },
  };
}
