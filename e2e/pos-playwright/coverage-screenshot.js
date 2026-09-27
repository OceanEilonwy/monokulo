// A UI stage for the coverage gallery. A whole page is captured in the
// shapes it is used in - a phone upright and on its side, a tablet (iPad Air
// size) upright and on its side, a desktop -
// then put back to the size the test had; anything smaller (a frame, one
// element) is captured once as it is.
const VIEWPORTS = [
  ['portrait', { width: 390, height: 844 }],
  ['landscape', { width: 844, height: 390 }],
  ['tablet-portrait', { width: 820, height: 1180 }],
  ['tablet-landscape', { width: 1180, height: 820 }],
  ['desktop', { width: 1280, height: 800 }],
];

async function captureCoverageStage(target, stage, testInfo) {
  if (process.env.COVERAGE_SCREENSHOTS !== '1') return;
  if (!/^[a-z]+-[a-z0-9-]+$/.test(stage)) throw new Error(`invalid coverage stage: ${stage}`);
  const isPage = typeof target.setViewportSize === 'function';
  if (!isPage) {
    const body = await target.screenshot({ animations: 'disabled', caret: 'hide' });
    await testInfo.attach(`coverage-stage:${stage}`, { body, contentType: 'image/png' });
    return;
  }
  const original = target.viewportSize();
  for (const [shape, size] of VIEWPORTS) {
    await target.setViewportSize(size);
    // Force layout at the new size (no timers: some tests fake the clock).
    await target.evaluate(() => document.documentElement.getBoundingClientRect().height);
    const body = await target.screenshot({ animations: 'disabled', caret: 'hide', fullPage: true });
    await testInfo.attach(`coverage-stage:${stage}-${shape}`, { body, contentType: 'image/png' });
  }
  if (original) await target.setViewportSize(original);
}

module.exports = { captureCoverageStage };
