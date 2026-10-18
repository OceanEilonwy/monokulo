// The Avatar component's behaviour (crates/ui/src/components/content.rs):
// the picture takes the letter's place only once it has loaded, so one that
// is missing or blocked leaves the letter, never a broken image.
import { createSignal } from 'solid-js';
import type { AvatarBehaviour, AvatarProps } from '../../solid/Avatar';

export function setup(_props: AvatarProps): AvatarBehaviour {
  const [loaded, setLoaded] = createSignal(false);
  return {
    loaded,
    load(event) {
      const image = event.currentTarget as HTMLImageElement;
      // An icon of no size (an empty file served as one) is no picture.
      setLoaded(image.naturalWidth > 0);
    },
  };
}
