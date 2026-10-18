// Copying for the copy components (CopyValue, GroupedValue): the Clipboard
// API where there is one; without it (a plain-http .onion) the selection,
// copied the old way where the browser still allows it, else left selected
// for a copy by hand, never a claim it copied.

// Selects everything inside `element`, focused: to read, or to copy by hand.
export function selectContents(element: HTMLElement) {
  element.focus();
  const range = document.createRange();
  range.selectNodeContents(element);
  const selection = window.getSelection();
  selection?.removeAllRanges();
  selection?.addRange(range);
}

// Copies `text`; `select` puts the same text in the selection for the
// fallback. Whether it was copied.
export async function copyText(text: string, select: () => void): Promise<boolean> {
  try {
    if (!navigator.clipboard?.writeText) throw new Error('Clipboard unavailable');
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    select();
    try {
      return document.execCommand('copy');
    } catch {
      return false;
    }
  }
}

// What to say when a copy couldn't be made: the value is selected.
export const COPY_BY_HAND = 'Select and copy the full value using your device’s copy command.';
