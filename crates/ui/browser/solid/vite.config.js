import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

// The Solid equivalence harness (index.html): renders every example from
// generated/fixtures.json through the generated Solid components. As in the
// POS's config, the plugin's own dev flag stays off (it sends an option the
// compiler no longer takes).
export default defineConfig({
  root: new URL('.', import.meta.url).pathname,
  plugins: [solid({ dev: false })],
});
