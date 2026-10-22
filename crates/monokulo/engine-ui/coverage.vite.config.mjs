import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';

const directory = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(directory, '../../..');
const requireFromTests = createRequire(path.join(root, 'e2e/browser/package.json'));
const { createInstrumenter } = requireFromTests('istanbul-lib-instrument');
// The engine page's authored sources, each counted under its repository path.
const sources = ['engine-view.js', 'events.tsx'];

function coverageInstrument() {
  return {
    name: 'coverage-authored-engine-view',
    enforce: 'post',
    apply: 'build',
    transform(code, id) {
      const file = sources.find(name => id.split('?')[0] === path.join(directory, 'src', name));
      if (!file) return null;
      const source = `crates/monokulo/engine-ui/src/${file}`;
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
    lib: { entry: 'src/engine-view.js', formats: ['es'], fileName: () => 'engine-view.js' },
  },
});
