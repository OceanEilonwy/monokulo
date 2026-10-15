import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';
import { readFileSync } from 'node:fs';
const escape = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
export default defineConfig({
  base: './',
  build: { rollupOptions: { input: { index: new URL('./index.html', import.meta.url).pathname, form: new URL('./form-page.html', import.meta.url).pathname, grouped: new URL('./form-grouped.html', import.meta.url).pathname } } },
  // The islands' behaviours are Solid (crates/ui/web/islands.ts). As in
  // the POS's config, the plugin's own dev flag stays off.
  plugins: [solid({ dev: false }), {
    name: 'static-component-examples',
    transformIndexHtml(html) {
      let assetIndex = 0;
      // An example may include another (a hand-written page around a
      // component crates/ui rendered: `generated/partials/...`).
      const read = name => readFileSync(new URL(`./examples/${name}.html`, import.meta.url), 'utf8').trim()
        .replace(/<!-- example:([a-z/-]+) -->/g, (_, inner) => read(inner));
      return html.replace(/<!-- (example|source):([a-z/-]+) -->/g, (_, mode, name) => {
        const markup = read(name).replace(/<!-- icon:([a-z-]+) -->/g, (_, icon) => readFileSync(new URL(`../../crates/ui/web/icons/${icon}.svg`, import.meta.url), 'utf8').trim()).replace(/<!-- asset:([a-z-]+)\/([a-z-]+) -->/g, (_, folder, name) => {
          let asset = readFileSync(new URL(`../../crates/ui/web/${folder}/${name}.svg`, import.meta.url), 'utf8').trim();
          // Each inline brand instance gets local clip IDs. Status sprites keep
          // their public IDs and appear once per document.
          if (folder === 'brand') {
            const suffix = `-${++assetIndex}`;
            const ids = [...asset.matchAll(/id="([^"]+)"/g)].map(match => match[1]);
            for (const id of ids) asset = asset.replaceAll(`id="${id}"`, `id="${id}${suffix}"`).replaceAll(`url(#${id})`, `url(#${id}${suffix})`);
          }
          return asset;
        });
        return mode === 'source' ? escape(markup) : markup;
      });
    },
    handleHotUpdate({ file, server }) {
      if (file.includes('/examples/') || file.includes('/crates/ui/web/') || file.includes('/crates/ui/solid/')) server.ws.send({ type: 'full-reload' });
    },
  }],
});
