import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

// The standalone build (web/bundle.js): the stylesheets and the islands
// script, which needs Solid's JSX transform.
export default defineConfig({
  plugins: [solid()],
  build: {
    lib: { entry: 'web/bundle.js', formats: ['es'], fileName: () => 'ui.js', cssFileName: 'ui' },
  },
});
