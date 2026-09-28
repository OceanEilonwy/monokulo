// A UI stage for the coverage gallery. A whole page is captured in the
// shapes it is used in - a phone upright and on its side, a small phone
// (320px, iPhone SE size) upright, a tablet (iPad Air size) upright and on
// its side, a desktop - then put back to the size the
// test had; anything smaller (a frame, one element) is captured once as it
// is.
//
// Each size is captured in the light and the dark theme: the page's own
// `data-theme` (and every frame's, so a framed checkout matches its page) is
// set for the shot, with the OS preference emulated to match, then put back.
//
// `options.group` names the page or feature the gallery files the stage
// under (`logs`, `pos-timeline`, ...); without it the gallery guesses from
// the spec's file name (checkout, pos, challenge). `options.shapes` limits
// the sizes: the POS timeline, read at a desk, is captured on desktop only. `options.themes` limits the themes the same way.
const SHAPES = {
  'mobile-portrait': { width: 390, height: 844 },
  'mobile-landscape': { width: 844, height: 390 },
  'small-portrait': { width: 320, height: 568 },
  'tablet-portrait': { width: 820, height: 1180 },
  'tablet-landscape': { width: 1180, height: 820 },
  desktop: { width: 1280, height: 800 },
};
const THEMES = ['light', 'dark'];
const NAME = /^[a-z]+-[a-z0-9-]+$/;

/** Shows every frame of `page` in `theme`, remembering what each had. */
async function showTheme(page, theme) {
  await page.emulateMedia({ colorScheme: theme });
  for (const frame of page.frames()) {
    await frame.evaluate((theme) => {
      const root = document.documentElement;
      if (!root.hasAttribute('data-coverage-theme')) root.setAttribute('data-coverage-theme', root.getAttribute('data-theme') || '');
      root.setAttribute('data-theme', theme);
    }, theme).catch(() => {});
  }
}

/** Puts back what `showTheme` changed. */
async function restoreTheme(page, colorScheme) {
  await page.emulateMedia({ colorScheme });
  for (const frame of page.frames()) {
    await frame.evaluate(() => {
      const root = document.documentElement;
      if (!root.hasAttribute('data-coverage-theme')) return;
      const was = root.getAttribute('data-coverage-theme');
      if (was) root.setAttribute('data-theme', was); else root.removeAttribute('data-theme');
      root.removeAttribute('data-coverage-theme');
    }).catch(() => {});
  }
}
const GROUP = /^[a-z][a-z0-9-]*$/;

async function captureCoverageStage(target, stage, testInfo, options = {}) {
  if (process.env.COVERAGE_SCREENSHOTS !== '1') return;
  if (!NAME.test(stage)) throw new Error(`invalid coverage stage: ${stage}`);
  const group = options.group || '';
  if (group && !GROUP.test(group)) throw new Error(`invalid coverage group: ${group}`);
  const shapes = options.shapes || Object.keys(SHAPES);
  for (const shape of shapes) if (!SHAPES[shape]) throw new Error(`unknown screenshot shape: ${shape}`);
  const themes = options.themes || THEMES;
  for (const theme of themes) if (!THEMES.includes(theme)) throw new Error(`unknown screenshot theme: ${theme}`);
  // `coverage-stage:<group>/<stage>@<shape>`, plus `+dark` for the dark
  // theme, read back by coverage-gallery-reporter.js.
  const name = (shape, theme = 'light') => `coverage-stage:${group ? `${group}/` : ''}${stage}@${shape}${theme === 'light' ? '' : `+${theme}`}`;
  const isPage = typeof target.setViewportSize === 'function';
  if (!isPage) {
    const body = await target.screenshot({ animations: 'disabled', caret: 'hide' });
    await testInfo.attach(name('element'), { body, contentType: 'image/png' });
    return;
  }
  const original = target.viewportSize();
  const colorScheme = await target.evaluate(() => (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'));
  for (const shape of shapes) {
    await target.setViewportSize(SHAPES[shape]);
    for (const theme of themes) {
      await showTheme(target, theme);
      // Force layout at the new size and theme (no timers: some tests fake
      // the clock).
      await target.evaluate(() => document.documentElement.getBoundingClientRect().height);
      const body = await target.screenshot({ animations: 'disabled', caret: 'hide', fullPage: true });
      await testInfo.attach(name(shape, theme), { body, contentType: 'image/png' });
    }
  }
  await restoreTheme(target, colorScheme);
  if (original) await target.setViewportSize(original);
}

module.exports = { captureCoverageStage, SHAPES, THEMES };
