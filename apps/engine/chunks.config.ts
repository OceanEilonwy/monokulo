
// What the build makes, shared by vite.config.ts and the coverage build
// (both run from this folder: the paths are from here).
// The entries keep their names: monokulo links them by name and puts its
// own digest in the URL (crate::assets). Every other chunk carries Vite's
// hash in its name, and monokulo serves it at that name: an entry imports
// it from next to itself.
export const entries = {
  'ui-islands': '../../crates/ui/web/islands.ts',
  'engine-view': 'src/engine-view.tsx',
};

export const chunks = {
  entryFileNames: '[name].js',
  chunkFileNames: '[name]-[hash].js',
  // The engine page's stylesheet (src/engine.css), linked by name like
  // the entries.
  assetFileNames: '[name][extname]',
  // Solid on its own: every page with islands needs it, and the browser
  // keeps it while the behaviours and the engine page change.
  // Vite's helper that fetches a behaviour's chunks together stays out of
  // it, so a page whose islands need no Solid doesn't load it.
  manualChunks: (id: string) => {
    if (/[\\/]node_modules[\\/](solid-js|@solidjs)[\\/]/.test(id)) return 'solid';
    if (id.includes('vite/preload-helper')) return 'preload';
    return undefined;
  },
};
