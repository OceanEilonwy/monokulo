const path = require('node:path');
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

// Chromium's fake camera plays fixtures/refund-qr-camera.y4m: the customer
// holding their wallet's receive QR up to the phone. Launch options, so
// this suite has its own file.
test.use({
  launchOptions: { args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
    `--use-file-for-fake-video-capture=${path.join(__dirname, '../fixtures/refund-qr-camera.y4m')}`] },
});

const address = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
let fixture;

test.beforeAll(async () => { fixture = await startCoverageFixture(); });
test.afterAll(async () => { await stopCoverageFixture(fixture?.process); });
test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

async function checkoutUrl(request) {
  const response = await request.post(`${fixture.base_url}/__coverage/orders`);
  expect(response.ok()).toBeTruthy();
  const { order_id } = await response.json();
  return `${fixture.base_url}/checkout/${order_id}`;
}

test('real checkout scans a refund QR with the camera, then stops the camera', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  // The decoder finds nothing until the test lets it (window.__decode), so
  // the camera is still looking when the customer first taps Stop.
  await page.addInitScript(() => {
    let real;
    Object.defineProperty(window, 'jsQR', { configurable: true,
      get: () => (real ? (...args) => (window.__decode ? real(...args) : null) : undefined),
      set: value => { real = value; } });
  });
  await page.goto(url);
  const scan = page.locator('#scan-refund');
  const video = page.locator('#refund-camera');

  // Stopped before it finds a code: the field is left as it was.
  await scan.click();
  await expect(scan).toHaveAttribute('aria-label', 'Stop camera');
  await expect(video).toBeVisible();
  const tracks = await video.evaluate(el => el.srcObject.getTracks().length);
  expect(tracks).toBeGreaterThan(0);
  await scan.click();
  await expect(scan).toHaveAttribute('aria-label', 'Scan refund QR');
  await expect(video).toBeHidden();
  await expect(page.locator('#refund_address')).toHaveValue('');

  // Looking again, it reads the code and saves it.
  await page.evaluate(() => { window.__decode = true; });
  await scan.click();
  await expect(page.locator('#refund_address')).toHaveValue(address);
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  // A successful scan turns the camera off by itself.
  await expect(video).toBeHidden();
  await expect(scan).toHaveAttribute('aria-label', 'Scan refund QR');
  await expect.poll(() => video.evaluate(el => el.srcObject)).toBeNull();
});
