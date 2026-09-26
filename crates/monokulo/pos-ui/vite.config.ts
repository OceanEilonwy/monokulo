import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

export default defineConfig({
  plugins: [solid()],
  build: {
    outDir: '../static',
    emptyOutDir: false,
    assetsDir: '.',
    lib: { entry: 'src/main.tsx', formats: ['es'], fileName: () => 'pos-app.js' },
    cssCodeSplit: false,
    rollupOptions: { output: { assetFileNames: 'pos-app[extname]' } },
  },
});
