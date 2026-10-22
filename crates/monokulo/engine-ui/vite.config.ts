import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

// The engine page's script (src/engine-view.js), built by
// crates/monokulo/build.rs into Cargo's OUT_DIR (`--outDir`): `--mode
// development` for debug builds (Solid's development build, with its
// diagnostics, unminified), `--mode production` for release builds. The
// compiler options are the POS's; pos-ui/vite.config.ts says why.
export default defineConfig(({ mode }) => {
  const development = mode === 'development';
  return {
    plugins: [solid(development ? { dev: false, solid: { dev: true, sourceNames: true } } : {})],
    resolve: development ? { conditions: ['solid', 'development', 'browser'] } : undefined,
    define: { 'process.env.NODE_ENV': JSON.stringify(mode) },
    build: {
      outDir: 'dist',
      emptyOutDir: true,
      minify: !development,
      lib: { entry: 'src/engine-view.js', formats: ['es'], fileName: () => 'engine-view.js' },
    },
  };
});
