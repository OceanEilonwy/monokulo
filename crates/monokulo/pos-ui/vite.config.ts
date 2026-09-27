import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

// Built by crates/monokulo/build.rs into Cargo's OUT_DIR (`--outDir`):
// `--mode development` for debug builds (Solid's development build, with its
// diagnostics, unminified), `--mode production` for release builds.
export default defineConfig(({ mode }) => {
  const development = mode === 'development';
  return {
    plugins: [solid({ dev: development })],
    resolve: development ? { conditions: ['development', 'browser'] } : undefined,
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) },
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      assetsDir: '.',
      minify: !development,
      lib: { entry: 'src/main.tsx', formats: ['es'], fileName: () => 'pos-app.js' },
      cssCodeSplit: false,
      rollupOptions: { output: { assetFileNames: 'pos-app[extname]' } },
    },
  };
});
