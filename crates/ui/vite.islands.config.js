import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

// The islands' script for server-rendered pages (web/islands.ts): Solid and
// every interactive component's behaviour, one ES module. A server that
// renders the components builds it (monokulo's build.rs) and serves it;
// `--mode development` keeps Solid's diagnostics, unminified.
export default defineConfig(({ mode }) => {
  const development = mode === 'development';
  return {
    root: new URL('.', import.meta.url).pathname,
    // As in the POS's config: the plugin's own dev flag stays off, Solid's
    // development build comes from the resolve conditions.
    plugins: [solid(development ? { dev: false, solid: { dev: true, sourceNames: true } } : {})],
    resolve: development ? { conditions: ['solid', 'development', 'browser'] } : undefined,
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) },
    build: {
      outDir: 'dist/islands',
      emptyOutDir: true,
      minify: !development,
      lib: { entry: 'web/islands.ts', formats: ['es'], fileName: () => 'ui-islands.js' },
    },
  };
});
