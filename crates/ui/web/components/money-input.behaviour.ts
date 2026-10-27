// The MoneyInput component's behaviour (crates/ui/src/components/forms.rs):
// the amount grouped as it's typed, a typed comma taken as the point, a
// paste or a drop read for what it means (or asked about when it can't be
// told), and the plain number put back in what the form sends. Nothing
// here drops a digit or rereads a comma it didn't write itself: anything
// it can't be sure of is left exactly as it is, and the field says why.
import type { MoneyInputBehaviour, MoneyInputProps } from '../../solid/MoneyInput';
import { GROUP, POINT, above, caretAfter, format, marksBefore, places, plain, readPasted } from './money-mask';

export function setup(props: MoneyInputProps): MoneyInputBehaviour {
  return {
    input(element) {
      attach(element as HTMLInputElement, props);
    },
  };
}

// The inputs attached, and those holding text that isn't a plain amount.
const attached = new WeakSet<HTMLInputElement>();
const heldAsTyped = new WeakSet<HTMLInputElement>();

// What any form sends is the plain number, with JavaScript as without: one
// listener for the page, so a form a script builds or moves an input into
// (fixi) sends it plain too. Text held as typed goes as it is, for the
// server to refuse, never as a guess.
document.addEventListener('formdata', event => {
  for (const el of Array.from((event.target as HTMLFormElement).elements)) {
    if (!(el instanceof HTMLInputElement) || !attached.has(el)) continue;
    if (el.disabled || !el.name || heldAsTyped.has(el)) continue;
    event.formData.set(el.name, plain(el.value));
  }
}, true);

const commas = (text: string) => text.split(GROUP).length - 1;
const ambiguous = (text: string) => `Is "${text}" a comma for thousands, or for decimals? Type it with a dot for decimals.`;
const notDigits = 'Enter the amount in digits, with a dot for decimals (1250.50).';

function attach(input: HTMLInputElement, props: MoneyInputProps) {
  const decimals = props.decimals ?? 2;
  attached.add(input);
  // The text as this behaviour last wrote it: its commas are the only ones
  // it may take as grouping.
  let written = input.value;
  let asTyped = false;
  // Text that isn't a plain amount stays as it is until it is one.
  const hold = (problem: string) => {
    asTyped = true;
    heldAsTyped.add(input);
    input.setCustomValidity(problem);
  };
  const release = () => {
    asTyped = false;
    heldAsTyped.delete(input);
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
  // Pasted or dropped text, read for what it means, or asked about.
  const insertRead = (text: string) => {
    const value = readPasted(text);
    if (value === undefined) {
      input.setCustomValidity(ambiguous(text.trim()));
      input.reportValidity();
      return;
    }
    const [start, end] = caret();
    put(value, start, end);
  };

  input.addEventListener('beforeinput', event => {
    if (asTyped) return;
    const [start, end] = caret();
    if (event.inputType === 'insertText' && event.data) {
      if (/^\d+$/.test(event.data)) {
        // A full fraction takes no more digits, rather than dropping one.
        const point = input.value.indexOf(POINT);
        if (start === end && point >= 0 && start > point && places(input.value) >= decimals) event.preventDefault();
        return;
      }
      event.preventDefault();
      // Either mark starts the fraction: a comma is never grouping typed by hand.
      const selected = input.value.slice(start, end);
      const pointElsewhere = input.value.includes(POINT) && !selected.includes(POINT);
      if ((event.data === ',' || event.data === '.') && decimals > 0 && !pointElsewhere) put(POINT, start, end);
      return;
    }
    // Text from a drop, a correction or anywhere else is read like a paste.
    if (event.inputType.startsWith('insert') && event.inputType !== 'insertCompositionText' && event.inputType !== 'insertLineBreak') {
      const text = event.data ?? event.dataTransfer?.getData('text') ?? '';
      if (text === '' || /^\d+$/.test(text)) return;
      event.preventDefault();
      insertRead(text);
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
    event.preventDefault();
    insertRead(event.clipboardData?.getData('text') ?? '');
  });

  input.addEventListener('input', () => {
    if (asTyped) {
      if (/[^0-9.]/.test(input.value)) return;
      release();
    }
    // A comma it didn't write (a keyboard's own composing, autofill, a
    // script) or anything but digits: kept as it is, and asked about.
    if (commas(input.value) > commas(written)) return hold(ambiguous(input.value));
    if (/[^0-9.,]/.test(input.value)) return hold(notDigits);
    const marks = marksBefore(input.value, caret()[0]);
    const next = format(input.value, decimals, true);
    if (next !== input.value) {
      input.value = next;
      const at = caretAfter(next, marks);
      input.setSelectionRange(at, at);
    }
    written = next;
    check();
  });

  // Done typing, or put back by a script: written out in full.
  const settle = () => {
    if (asTyped) return;
    if (/[^0-9.,]/.test(input.value) || commas(input.value) > commas(written)) {
      hold(/,/.test(input.value) ? ambiguous(input.value) : notDigits);
      return;
    }
    input.value = format(input.value, decimals);
    written = input.value;
    check();
  };
  input.addEventListener('change', settle);
  input.addEventListener('blur', settle);

  // The server's value is plain; one it sent back as it was typed (a
  // refused `1,250` or `5e1`) is left exactly as it is, to be put right.
  const value = input.value.trim();
  if (/[^0-9.]/.test(value)) {
    hold(/,/.test(value) ? ambiguous(value) : notDigits);
    return;
  }
  written = '';
  if (document.activeElement !== input) settle();
  else check();
}
