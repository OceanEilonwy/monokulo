const path = require('node:path');
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

// Chromium's fake camera plays this file: the customer holding their
// wallet's receive QR up to the phone. A launch option, so this suite has
// its own file.
test.use({ launchOptions: { args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
  `--use-file-for-fake-video-capture=${path.join(__dirname, '../fixtures/refund-qr-camera.y4m')}`] } });

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
  return `${fixture.base_url}/pay/${fixture.public_key}/orders/${order_id}`;
}

test('real checkout scans a refund QR with the camera, then stops the camera', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.goto(url);
  const scan = page.locator('#scan-refund');
  const video = page.locator('#refund-camera');
  await scan.click();
  await expect(page.locator('#refund_address')).toHaveValue(address);
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  // A successful scan turns the camera off by itself.
  await expect(video).toBeHidden();
  await expect(scan).toHaveAttribute('aria-label', 'Scan refund QR');
  expect(await video.evaluate(el => el.srcObject)).toBeNull();
});

test('real checkout camera can be stopped by the customer before it finds a code', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  // Hold off the decoder so the camera is still looking when they tap Stop.
  await page.addInitScript(() => {
    let real;
    Object.defineProperty(window, 'jsQR', { configurable: true,
      get: () => (real ? (...args) => (window.__decode ? real(...args) : null) : undefined),
      set: value => { real = value; } });
  });
  await page.goto(url);
  const scan = page.locator('#scan-refund');
  await scan.click();
  await expect(scan).toHaveAttribute('aria-label', 'Stop camera');
  await expect(page.locator('#refund-camera')).toBeVisible();
  const tracks = await page.locator('#refund-camera').evaluate(el => el.srcObject.getTracks().length);
  expect(tracks).toBeGreaterThan(0);
  await scan.click();
  await expect(scan).toHaveAttribute('aria-label', 'Scan refund QR');
  await expect(page.locator('#refund-camera')).toBeHidden();
  await expect(page.locator('#refund_address')).toHaveValue('');
});
