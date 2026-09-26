import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

const directory = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(directory, '../../..');
const requireFromTests = createRequire(path.join(root, 'e2e/pos-playwright/package.json'));
const { createInstrumenter } = requireFromTests('istanbul-lib-instrument');
const source = 'crates/monokulo/pos-ui/src/main.tsx';

function coverageInstrument() {
  return {
    name: 'coverage-authored-pos',
    enforce: 'post',
    apply: 'build',
    transform(code, id) {
      if (id.split('?')[0] !== path.join(directory, 'src/main.tsx')) return null;
      const inputMap = this.getCombinedSourcemap();
      const instrumenter = createInstrumenter({ esModules: true, produceSourceMap: true, compact: false });
      const output = instrumenter.instrumentSync(code, source, inputMap.mappings ? inputMap : undefined);
      return { code: output, map: instrumenter.lastSourceMap() };
    },
  };
}

export default defineConfig({
  plugins: [solid(), coverageInstrument()],
  build: {
    outDir: path.join(root, 'target/coverage/browser/assets'),
    emptyOutDir: false,
    sourcemap: true,
    minify: false,
    assetsDir: '.',
    lib: { entry: 'src/main.tsx', formats: ['es'], fileName: () => 'pos-app.js' },
    cssCodeSplit: false,
    rollupOptions: { output: { assetFileNames: 'pos-app[extname]' } },
  },
});
