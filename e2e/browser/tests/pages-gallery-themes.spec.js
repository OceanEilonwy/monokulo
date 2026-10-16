// @ts-check
// The Pages site's screenshot gallery (xtask/src/pages/render/screens.rs,
// assets/styles.css): each card shows exactly one thumbnail per frame, the
// one in the theme picked, and with no theme picked the one matching the
// reader's colour scheme. A shape taken in one theme only shows that lone
// picture whatever is picked. The markup is the card's frame as screens.rs
// writes it; the stylesheet is the site's own.
const fs = require('node:fs');
const path = require('node:path');
const { test, expect } = require('../coverage-test');

const STYLES = fs.readFileSync(path.join(__dirname, '../../../xtask/src/pages/assets/styles.css'), 'utf8');
const PIXEL = 'data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=';

/** A gallery in `theme` with one card: a frame with both pictures, and one
 * with only a light picture. */
const gallery = (theme) => `<style>${STYLES}</style>
  <div class="gallery" id="gallery" data-device="main" data-theme="${theme}">
    <a class="gcard"><span class="frame main" id="pair">
      <img class="light" src="${PIXEL}" width="480" height="330" alt="light">
      <img class="dark" src="${PIXEL}" width="480" height="330" alt="dark">
    </span></a>
    <a class="gcard"><span class="frame main" id="lone">
      <img class="light" src="${PIXEL}" width="480" height="330" alt="light">
    </span></a>
  </div>`;

/** The alt text of the pictures `frame` shows. */
const shown = (page, frame) => page.locator(`#${frame} img`).evaluateAll(
  (imgs) => imgs.filter((i) => getComputedStyle(i).display !== 'none').map((i) => i.getAttribute('alt')));

for (const scheme of /** @type {const} */ (['light', 'dark'])) {
  test(`a ${scheme} reader sees one thumbnail per frame in every theme mode`, async ({ page }) => {
    await page.emulateMedia({ colorScheme: scheme });
    const expected = { auto: [scheme], light: ['light'], dark: ['dark'], both: ['light', 'dark'] };
    for (const [theme, pictures] of Object.entries(expected)) {
      await page.setContent(gallery(theme));
      expect(await shown(page, 'pair'), `${theme} mode`).toEqual(pictures);
      expect(await shown(page, 'lone'), `${theme} mode, light shot only`).toEqual(['light']);
    }
  });
}
