import { defineConfig } from 'vite';
export default defineConfig({
  build: {
    lib: { entry: 'web/bundle.js', formats: ['es'], fileName: () => 'ui.js', cssFileName: 'ui' },
  },
});
