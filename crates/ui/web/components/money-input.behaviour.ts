// The MoneyInput component's behaviour (crates/ui/src/components/forms.rs):
// the amount grouped as it's typed, a typed comma taken as the point, a
// paste read for what it means (or asked about when it can't be told),
// and the plain number put back in what the form sends. The input's own
// value is only ever how it's written; the server's is always plain.
import type { MoneyInputBehaviour, MoneyInputProps } from '../../solid/MoneyInput';
import { GROUP, POINT, above, caretAfter, format, marksBefore, places, plain, readPasted } from './money-mask';

export function setup(props: MoneyInputProps): MoneyInputBehaviour {
  return {
    input(element) {
      attach(element as HTMLInputElement, props);
    },
  };
}

// The inputs attached, and those still holding the server's ambiguous text.
const attached = new WeakSet<HTMLInputElement>();
const ambiguousText = new WeakSet<HTMLInputElement>();

// What any form sends is the plain number, with JavaScript as without: one
// listener for the page, so a form a script builds or moves an input into
// (fixi) sends it plain too. Ambiguous text goes as it is, for the server to
// refuse, never as a guess.
document.addEventListener('formdata', event => {
  for (const el of Array.from((event.target as HTMLFormElement).elements)) {
    if (!(el instanceof HTMLInputElement) || !attached.has(el)) continue;
    if (el.disabled || !el.name || ambiguousText.has(el)) continue;
    event.formData.set(el.name, plain(el.value));
  }
}, true);

function attach(input: HTMLInputElement, props: MoneyInputProps) {
  const decimals = props.decimals ?? 2;
  attached.add(input);
  // Still the server's ambiguous text: not written out until it is plain.
  let asTyped = false;
  const holdAsTyped = (on: boolean) => {
    asTyped = on;
    if (on) ambiguousText.add(input);
    else ambiguousText.delete(input);
  };
  const caret = () => [input.selectionStart ?? input.value.length, input.selectionEnd ?? input.value.length] as const;
  // Says what's wrong before the form is sent; the server still checks.
  const check = () => {
    const value = plain(input.value);
    let problem = '';
    if (value && places(value) > decimals) {
      problem = decimals === 0 ? 'Enter a whole number.' : `Enter at most ${decimals} decimal places.`;
    } else if (value && props.max && above(value, props.max)) {
      problem = props.over_max ?? `Enter at most ${format(props.max, decimals)}.`;
    }
    input.setCustomValidity(problem);
  };
  // `text` in place of the selection, as if typed: the input event writes it out.
  const put = (text: string, start: number, end: number) => {
    input.setRangeText(text, start, end, 'end');
    input.dispatchEvent(new Event('input', { bubbles: true }));
  };

  input.addEventListener('beforeinput', event => {
    const [start, end] = caret();
    if (event.inputType === 'insertText' && event.data) {
      if (/^\d+$/.test(event.data)) return;
      event.preventDefault();
      // Either mark starts the fraction: a comma is never grouping typed by hand.
      const selected = input.value.slice(start, end);
      const pointElsewhere = input.value.includes(POINT) && !selected.includes(POINT);
      if ((event.data === ',' || event.data === '.') && decimals > 0 && !pointElsewhere) put(POINT, start, end);
      return;
    }
    // Deleting a group comma deletes the digit beyond it instead.
    if (start === end && event.inputType === 'deleteContentBackward' && input.value[start - 1] === GROUP) {
      event.preventDefault();
      put('', start - 2, start - 1);
    } else if (start === end && event.inputType === 'deleteContentForward' && input.value[start] === GROUP) {
      event.preventDefault();
      put('', start + 1, start + 2);
    }
  });

  input.addEventListener('paste', event => {
    const text = event.clipboardData?.getData('text') ?? '';
    event.preventDefault();
    const value = readPasted(text);
    if (value === undefined) {
      input.setCustomValidity(ambiguous(text.trim()));
      input.reportValidity();
      return;
    }
    const [start, end] = caret();
    put(value, start, end);
  });

  input.addEventListener('input', () => {
    if (asTyped) {
      if (/[^0-9.]/.test(input.value)) return;
      holdAsTyped(false);
    }
    const marks = marksBefore(input.value, caret()[0]);
    const next = format(input.value, decimals, true);
    if (next !== input.value) {
      input.value = next;
      const at = caretAfter(next, marks);
      input.setSelectionRange(at, at);
    }
    check();
  });

  // Done typing, or put back by a script: written out in full.
  const settle = () => {
    if (asTyped) return;
    input.value = format(input.value, decimals);
    check();
  };
  input.addEventListener('change', settle);
  input.addEventListener('blur', settle);

  // A value the server sent back as it was typed (a refused `1,250`) is
  // read as a paste would be, or left alone and asked about: never reread
  // with its comma taken as grouping.
  const ambiguous = (text: string) => `Is "${text}" a comma for thousands, or for decimals? Type it with a dot for decimals.`;
  if (/[^0-9.]/.test(input.value.trim())) {
    const read = readPasted(input.value);
    if (read === undefined) {
      holdAsTyped(true);
      input.setCustomValidity(ambiguous(input.value.trim()));
      return;
    }
    input.value = read;
  }
  if (document.activeElement !== input) settle();
  else check();
}
