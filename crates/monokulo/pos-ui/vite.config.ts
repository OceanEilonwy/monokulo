import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

// Built by crates/monokulo/build.rs into Cargo's OUT_DIR (`--outDir`):
// `--mode development` for debug builds (Solid's development build, with its
// diagnostics, unminified), `--mode production` for release builds.
export default defineConfig(({ mode }) => {
  const development = mode === 'development';
  return {
    // Development compiles through the `solid` compiler options rather than
    // the plugin's own `dev` flag: @solidjs/compiler 2.0.0-rc.10 renamed the
    // plugin's `componentNames` option to `sourceNames` and rejects the old
    // name, and @solidjs/vite-plugin 3.0.0-next.44 (the newest) still sends
    // it in dev. `dev` + `sourceNames` here produce the same output; switch
    // back to `solid({ dev: development })` once the plugin sends
    // `sourceNames` itself.
    plugins: [solid(development ? { dev: false, solid: { dev: true, sourceNames: true } } : {})],
    // The development runtime (Solid's diagnostics) - what the plugin's dev
    // flag would otherwise select.
    resolve: development ? { conditions: ['solid', 'development', 'browser'] } : undefined,
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
