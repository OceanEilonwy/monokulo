// The CopyBlock component's behaviour (crates/ui/src/components/copy.rs):
// a Copy button for the block's text as it is at the click, less what
// follows the comment marker on each line. Where the clipboard is refused,
// the text is selected to copy by hand.
import { createSignal } from 'solid-js';
import type { CopyBlockBehaviour, CopyBlockProps } from '../../solid/CopyBlock';

export function setup(props: CopyBlockProps): CopyBlockBehaviour {
  let code: HTMLElement | undefined;
  const [status, setStatus] = createSignal('');
  const [label, setLabel] = createSignal('Copy');

  const text = () => {
    const all = code?.textContent ?? '';
    const marker = props.comment;
    if (!marker) return all;
    return all.split('\n').map(line => {
      const at = line.indexOf(marker);
      return at < 0 ? line : line.slice(0, at).trimEnd();
    }).join('\n');
  };

  return {
    label,
    status,
    async copy() {
      try {
        if (!navigator.clipboard?.writeText) throw new Error('Clipboard unavailable');
        await navigator.clipboard.writeText(text());
        setStatus('Copied');
        setLabel('Copied');
        setTimeout(() => setLabel('Copy'), 1600);
      } catch {
        if (code) {
          const range = document.createRange();
          range.selectNodeContents(code);
          const selection = window.getSelection();
          selection?.removeAllRanges();
          selection?.addRange(range);
        }
        setStatus('Select and copy it using your device’s copy command.');
      }
    },
    code(element) {
      code = element;
    },
  };
}
