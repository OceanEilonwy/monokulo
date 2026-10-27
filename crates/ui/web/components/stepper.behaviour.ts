// The Stepper component's behaviour (crates/ui/src/components/forms.rs):
// − and + (and the arrow keys) take one off and add one, from what's typed,
// each stopping at its end. A step is an `input` and a `change` event on the input, as typing
// is, so a settings form counts it.
import { createSignal } from 'solid-js';
import type { StepperBehaviour, StepperProps } from '../../solid/Stepper';

export function setup(props: StepperProps): StepperBehaviour {
  // Set as the behaviour attaches, from the input's own value: `ownedWrite`.
  const [atMin, setAtMin] = createSignal(false, { ownedWrite: true });
  const [atMax, setAtMax] = createSignal(false, { ownedWrite: true });
  let input: HTMLInputElement | undefined;
  const min = () => props.min ?? 0;

  const sync = () => {
    if (!input) return;
    const n = Number(input.value);
    const known = input.value.trim() !== '' && Number.isFinite(n);
    setAtMin(input.disabled || (known && n <= min()));
    setAtMax(input.disabled || (known && props.max !== undefined && n >= props.max));
    if (known) input.setAttribute('aria-valuenow', String(n));
    else input.removeAttribute('aria-valuenow');
  };

  const step = (by: number) => {
    if (!input) return;
    const n = Number(input.value);
    const from = input.value.trim() === '' || !Number.isFinite(n) ? min() - by : Math.round(n);
    let next = Math.max(min(), from + by);
    if (props.max !== undefined) next = Math.min(props.max, next);
    input.value = String(next);
    input.dispatchEvent(new Event('input', { bubbles: true }));
    input.dispatchEvent(new Event('change', { bubbles: true }));
    sync();
  };

  return {
    at_min: atMin,
    at_max: atMax,
    down: () => step(-1),
    up: () => step(1),
    input(element) {
      input = element as HTMLInputElement;
      input.addEventListener('input', sync);
      input.addEventListener('change', sync);
      // The arrow keys step it, as a spin button's do.
      input.addEventListener('keydown', event => {
        if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
          event.preventDefault();
          step(event.key === 'ArrowUp' ? 1 : -1);
        }
      });
      sync();
    },
  };
}
