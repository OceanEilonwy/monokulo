// A UI stage for the coverage gallery. A whole page is captured in the
// shapes it is used in - a phone upright and on its side, a tablet (iPad Air
// size) upright and on its side, a desktop - then put back to the size the
// test had; anything smaller (a frame, one element) is captured once as it
// is.
//
// `options.group` names the page or feature the gallery files the stage
// under (`logs`, `pos-timeline`, ...); without it the gallery guesses from
// the spec's file name (checkout, pos, challenge). `options.shapes` limits
// the sizes: the Logs page, an admin tool used at a desk, is captured on
// desktop only.
const SHAPES = {
  'mobile-portrait': { width: 390, height: 844 },
  'mobile-landscape': { width: 844, height: 390 },
  'tablet-portrait': { width: 820, height: 1180 },
  'tablet-landscape': { width: 1180, height: 820 },
  desktop: { width: 1280, height: 800 },
};
const NAME = /^[a-z]+-[a-z0-9-]+$/;
const GROUP = /^[a-z][a-z0-9-]*$/;

async function captureCoverageStage(target, stage, testInfo, options = {}) {
  if (process.env.COVERAGE_SCREENSHOTS !== '1') return;
  if (!NAME.test(stage)) throw new Error(`invalid coverage stage: ${stage}`);
  const group = options.group || '';
  if (group && !GROUP.test(group)) throw new Error(`invalid coverage group: ${group}`);
  const shapes = options.shapes || Object.keys(SHAPES);
  for (const shape of shapes) if (!SHAPES[shape]) throw new Error(`unknown screenshot shape: ${shape}`);
  // `coverage-stage:<group>/<stage>@<shape>`, read back by
  // coverage-gallery-reporter.js.
  const name = shape => `coverage-stage:${group ? `${group}/` : ''}${stage}@${shape}`;
  const isPage = typeof target.setViewportSize === 'function';
  if (!isPage) {
    const body = await target.screenshot({ animations: 'disabled', caret: 'hide' });
    await testInfo.attach(name('element'), { body, contentType: 'image/png' });
    return;
  }
  const original = target.viewportSize();
  for (const shape of shapes) {
    await target.setViewportSize(SHAPES[shape]);
    // Force layout at the new size (no timers: some tests fake the clock).
    await target.evaluate(() => document.documentElement.getBoundingClientRect().height);
    const body = await target.screenshot({ animations: 'disabled', caret: 'hide', fullPage: true });
    await testInfo.attach(name(shape), { body, contentType: 'image/png' });
  }
  if (original) await target.setViewportSize(original);
}

module.exports = { captureCoverageStage, SHAPES };
