import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

// The islands' script for the GitHub Pages site (web/islands.ts): Solid and
// every interactive component's behaviour, one ES module, as `cargo xtask
// pages` publishes it: the behaviours come from the registry that holds
// them all (solid/islands.eager.ts), not the one that loads each when a
// page first has it. Inlining that one's imports instead would wrap every
// module in its own initialiser, a larger file. monokulo
// builds the islands itself, with the engine page's script, in chunks
// (apps/engine/vite.config.ts). An app build rather than library mode,
// which leaves ES output's whitespace in; `--mode development` keeps
// Solid's diagnostics, unminified.
export default defineConfig(({ mode }) => {
  const development = mode === 'development';
  return {
    root: fileURLToPath(new URL('.', import.meta.url)),
    // As in the POS's config: the plugin's own dev flag stays off, Solid's
    // development build comes from the resolve conditions.
    plugins: [solid(development ? { dev: false, solid: { dev: true, sourceNames: true } } : {})],
    resolve: {
      alias: [{ find: /^\.\.\/solid\/islands$/, replacement: fileURLToPath(new URL('solid/islands.eager.ts', import.meta.url)) }],
      ...(development ? { conditions: ['solid', 'development', 'browser'] } : {}),
    },
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) },
    build: {
      outDir: 'dist/islands',
      emptyOutDir: true,
      minify: !development,
      modulePreload: false,
      rollupOptions: {
        input: { 'ui-islands': fileURLToPath(new URL('web/islands.ts', import.meta.url)) },
        preserveEntrySignatures: false,
        output: { entryFileNames: '[name].js' },
      },
    },
  };
});
