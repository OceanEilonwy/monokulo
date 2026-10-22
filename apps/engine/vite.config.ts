import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';
import { chunks, entries } from './chunks.config.ts';

// monokulo's server-rendered pages' scripts, built by
// crates/monokulo/build.rs into Cargo's OUT_DIR (`--outDir`): the shared
// components' islands (`ui-islands.js`, every page with a nav) and the
// engine page's live view (`engine-view.js`, src/engine-view.tsx). One build,
// so the engine page loads one copy of Solid with the islands rather than
// two. `--mode development` for debug builds (Solid's development build,
// with its diagnostics, unminified), `--mode production` for release builds.
// The compiler options are the POS's; apps/pos/vite.config.ts says why.
export default defineConfig(({ mode }) => {
  const development = mode === 'development';
  return {
    plugins: [solid(development ? { dev: false, solid: { dev: true, sourceNames: true } } : {})],
    resolve: development ? { conditions: ['solid', 'development', 'browser'] } : undefined,
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) },
    // Chunks are found next to the module that imports them.
    base: './',
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      minify: !development,
      // Which chunks each entry and behaviour needs (.vite/manifest.json):
      // build.rs lists them, and a page asks for them up front.
      manifest: true,
      rollupOptions: { input: entries, preserveEntrySignatures: false, output: chunks },
    },
  };
});
