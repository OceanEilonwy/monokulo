// The frames of a page a test helper can talk to. A `loading="lazy"` frame far
// below the fold has no document until it comes near the viewport, and until
// then `frame.evaluate` neither answers nor fails: it waits for the document,
// for good. Such a frame is the one whose `url()` is still empty.
const { expect } = require('@playwright/test');

/** The frames of `page` that have a document to evaluate in. */
function framesWithDocument(page) {
  return page.frames().filter((frame) => frame.url() !== '');
}

/** Loads every lazy frame of `page` now, wherever it is on the page, for a
 * full-page screenshot: taking one doesn't scroll, so it would show them empty.
 * They stay eager afterwards. */
async function loadLazyFrames(page) {
  for (const frame of framesWithDocument(page)) {
    await frame.evaluate(() => {
      for (const lazy of document.querySelectorAll('iframe[loading="lazy"]')) lazy.loading = 'eager';
    }).catch(() => {});
  }
  await expect.poll(() => page.frames().length - framesWithDocument(page).length).toBe(0);
  // A frame can go away while it's waited for, as in any page that moves on.
  await Promise.all(page.frames().map((frame) => frame.waitForLoadState('load').catch(() => {})));
}

module.exports = { framesWithDocument, loadLazyFrames };
