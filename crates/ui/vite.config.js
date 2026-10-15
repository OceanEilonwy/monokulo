import { defineConfig } from 'vite';
export default defineConfig({
  build: {
    lib: { entry: 'src/bundle.js', formats: ['es'], fileName: () => 'ui.js', cssFileName: 'ui' },
  },
});
