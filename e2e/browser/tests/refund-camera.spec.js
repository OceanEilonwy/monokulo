const fs = require('node:fs');
const path = require('node:path');
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

// Each browser's camera, as a test gets one. Chromium's fake device plays
// fixtures/refund-qr-camera.y4m: the customer holding their wallet's
// receive QR up to the phone. Firefox's fake device, and WebKit's mock one
// once the camera is allowed (as a customer allows it at the prompt), show
// a test pattern holding no code, so there the code is held up to it
// (holdUpCode). Launch options, so this suite has its own file.
const CAMERAS = {
  chromium: { showsCode: true, permissions: [], launchOptions: { args: ['--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream',
    `--use-file-for-fake-video-capture=${path.join(__dirname, '../fixtures/refund-qr-camera.y4m')}`] } },
  firefox: { showsCode: false, permissions: [],
    launchOptions: { firefoxUserPrefs: { 'media.navigator.streams.fake': true, 'media.navigator.permission.disabled': true } } },
  webkit: { showsCode: false, permissions: ['camera'], launchOptions: {} },
};
test.use({
  launchOptions: [async ({ browserName }, use) => use(CAMERAS[browserName].launchOptions), { scope: 'worker' }],
  permissions: async ({ browserName }, use) => use(CAMERAS[browserName].permissions),
});

/** Holds the refund QR up to a camera that shows none: the page still gets
 * the browser's own camera, asked for as it asks, but what the camera sees
 * is the code (fixtures/refund-qr.png on a white card). Stopping the
 * picture stops the camera. */
async function holdUpCode(page, browserName) {
  if (CAMERAS[browserName].showsCode) return;
  const code = `data:image/png;base64,${fs.readFileSync(path.join(__dirname, '../fixtures/refund-qr.png')).toString('base64')}`;
  await page.addInitScript(code => {
    const devices = navigator.mediaDevices;
    const camera = devices.getUserMedia.bind(devices);
    devices.getUserMedia = async constraints => {
      const device = await camera(constraints);
      const canvas = Object.assign(document.createElement('canvas'), { width: 640, height: 480 });
      const context = canvas.getContext('2d');
      const image = new Image();
      image.src = code;
      await image.decode();
      // A camera sends frames all the time; a canvas only when drawn.
      const draw = () => { context.fillStyle = 'white'; context.fillRect(0, 0, 640, 480); context.drawImage(image, 160, 80, 320, 320); };
      draw();
      const frames = setInterval(draw, 100);
      const [track] = canvas.captureStream(10).getVideoTracks();
      const stop = track.stop.bind(track);
      track.stop = () => { clearInterval(frames); stop(); device.getTracks().forEach(t => t.stop()); };
      return new MediaStream([track]);
    };
  }, code);
}

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

test('real checkout scans a refund QR with the camera, then stops the camera', async ({ page, request, browserName }) => {
  const url = await checkoutUrl(request);
  await holdUpCode(page, browserName);
  await page.goto(url);
  const scan = page.locator('#scan-refund');
  const video = page.locator('#refund-camera');
  await scan.click();
  await expect(page.locator('#refund_address')).toHaveValue(address);
  await expect(page.locator('#refund-field')).toHaveClass(/is-saved/);
  // A successful scan turns the camera off by itself.
  await expect(video).toBeHidden();
  await expect(scan).toHaveAttribute('aria-label', 'Scan refund QR');
  await expect.poll(() => video.evaluate(el => el.srcObject)).toBeNull();
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

test('merchant scans the customer refund QR with the POS tablet camera', async ({ page, context, browserName }) => {
  await holdUpCode(page, browserName);
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
  await page.goto(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`);
  const card = page.locator('.pos-pay-card');
  await card.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(card.locator('#pos-refund')).toHaveValue(address);
  await expect(card.locator('.pos-refund-state')).toHaveAttribute('aria-label', 'Refund address saved');
  // The scan ends by itself: camera hidden and released.
  await expect(card.locator('.pos-camera')).toBeHidden();
  await expect(card.getByRole('button', { name: 'Scan refund QR' })).toBeVisible();
  await expect.poll(() => card.locator('.pos-camera').evaluate(el => el.srcObject)).toBeNull();
});

test('POS tablet camera stopped before it finds a code leaves the field as it was', async ({ page, context }) => {
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
  // Hold off the decoder so the camera is still looking when they tap Stop.
  await page.addInitScript(() => {
    let real;
    Object.defineProperty(window, 'jsQR', { configurable: true, get: () => (real ? () => null : undefined), set: value => { real = value; } });
  });
  await page.goto(`${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`);
  const card = page.locator('.pos-pay-card');
  await expect(card).toBeVisible();
  const before = await card.locator('#pos-refund').inputValue();
  await card.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(card.getByRole('button', { name: 'Stop camera' })).toBeVisible();
  await expect(card.locator('.pos-camera')).toBeVisible();
  await card.getByRole('button', { name: 'Stop camera' }).click();
  await expect(card.locator('.pos-camera')).toBeHidden();
  await expect(card.locator('#pos-refund')).toHaveValue(before);
  await expect(card.locator('.pos-refund-message')).toHaveCount(0);
});
