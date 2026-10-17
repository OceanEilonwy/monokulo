import { defineConfig } from 'vite';
import solid from '@solidjs/vite-plugin';
import { readFileSync, readdirSync } from 'node:fs';
const escape = text => text.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;');
// On the Pages site (/stylebook/), the typeface comes from the site's own
// assets/ folder, which the landing page, docs and testing report share
// (xtask/src/pages/site_assets.rs): the same URLs, so a reader arriving
// from another part has the font cached rather than drawing the page in
// the fallback first. STYLEBOOK_SITE_ASSETS is that folder, already
// built; each weight is the file there with the same bytes as the ui
// package's.
const siteAssets = process.env.STYLEBOOK_SITE_ASSETS;
const font = /manrope-(\d+)-[^/]*\.woff2$/;
const siteFont = weight => {
  const bytes = readFileSync(new URL(`../../crates/ui/web/fonts/manrope-${weight}.woff2`, import.meta.url));
  const name = readdirSync(siteAssets).find(file => new RegExp(`^manrope-${weight}\\.[0-9a-f]{16}\\.woff2$`).test(file) && readFileSync(`${siteAssets}/${file}`).equals(bytes));
  if (!name) throw new Error(`${siteAssets} has no manrope-${weight} the same as the ui package's: build the landing page first`);
  return name;
};
export default defineConfig({
  base: './',
  experimental: siteAssets ? {
    // The stylesheet is in /stylebook/assets/, two folders below the site's.
    renderBuiltUrl(file, { hostType }) {
      const weight = file.match(font)?.[1];
      if (!weight) return;
      if (hostType !== 'css') throw new Error(`${file} is linked from ${hostType}, not the stylesheet`);
      return `../../assets/${siteFont(weight)}`;
    },
  } : {},
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
    // Its own copies of the typeface, which nothing links on the site.
    generateBundle(_, bundle) {
      if (siteAssets) for (const file of Object.keys(bundle)) if (font.test(file)) delete bundle[file];
    },
    handleHotUpdate({ file, server }) {
      if (file.includes('/examples/') || file.includes('/crates/ui/web/') || file.includes('/crates/ui/solid/')) server.ws.send({ type: 'full-reload' });
    },
  }],
});
