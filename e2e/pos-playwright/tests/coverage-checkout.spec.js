const path = require('node:path');
const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');

const address = '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC';
const qrImage = path.join(__dirname, '../fixtures/refund-qr.png');
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

test('real checkout saves a QR refund address and restores a saved address after editing', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.goto(url);
  const input = page.locator('#refund_address');
  const field = page.locator('#refund-field');
  await input.focus();
  await input.blur();
  await expect(field).not.toHaveClass(/is-saved|is-saving|is-invalid/);
  await expect(page.locator('#upload-refund')).toBeVisible();
  const chooser = page.waitForEvent('filechooser');
  await page.getByRole('button', { name: 'Choose QR image' }).click();
  await (await chooser).setFiles(qrImage);
  await expect(input).toHaveValue(address);
  await expect(field).toHaveClass(/is-saved/);
  await expect(page.locator('#refund-save-state')).toHaveAttribute('aria-label', 'Refund address saved');
  await page.reload();
  await expect(input).toHaveValue(address);
  await expect(field).toHaveClass(/is-saved/);
  await input.click();
  await expect(field).not.toHaveClass(/is-saved/);
  await input.fill('');
  await input.blur();
  await expect(input).toHaveValue(address);
  await expect(field).toHaveClass(/is-saved/);
});

test('real checkout validates, shows saving, rejects a response, and retries', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  let responseStatus = 400;
  let release;
  await page.route(`${url}/refund-address*`, async route => {
    if (responseStatus === 400) await new Promise(resolve => { release = resolve; });
    await route.fulfill({ status: responseStatus, contentType: 'application/json',
      body: JSON.stringify(responseStatus === 200 ? { ok: true } : { ok: false, error: 'invalid' }) });
  });
  await page.goto(url);
  const input = page.locator('#refund_address');
  const field = page.locator('#refund-field');
  await input.fill('short');
  await expect(field).not.toHaveClass(/is-saving|is-saved/);
  await input.blur();
  await expect(page.locator('#scan-error')).toContainText('95 or 106');
  await input.fill(address);
  await expect(field).toHaveClass(/is-saving/);
  release();
  await expect(field).toHaveClass(/is-invalid/);
  await expect(page.locator('#scan-error')).toContainText('Invalid refund address');
  responseStatus = 200;
  await input.fill(address.slice(0, -1) + 'D');
  await expect(field).not.toHaveClass(/is-invalid/);
  await expect(field).toHaveClass(/is-saved/);
});

test('real checkout keeps retry and camera upload paths after failures', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.addInitScript(() => {
    Object.defineProperty(navigator.mediaDevices, 'getUserMedia', { value: async () => { throw new Error('denied'); } });
    Object.defineProperty(navigator.mediaDevices, 'enumerateDevices', { value: async () => [] });
  });
  await page.route(`${url}/refund-address*`, route => route.abort('failed'));
  await page.goto(url);
  await expect(page.locator('#scan-refund')).toBeVisible();
  await page.getByRole('button', { name: 'Scan refund QR' }).click();
  await expect(page.locator('#scan-error')).toContainText('Camera unavailable');
  await expect(page.locator('#upload-refund')).toBeVisible();
  await page.locator('#refund_address').fill(address);
  await expect(page.locator('#scan-error')).toContainText('Failed to fetch');
  await expect(page.locator('#refund-field')).not.toHaveClass(/is-saved/);
});

test('real checkout copies and selects payment address at narrow and wide widths', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.goto(url);
  const input = page.locator('#address');
  await input.dblclick();
  expect(await input.evaluate(el => [el.selectionStart, el.selectionEnd])).toEqual([0, (await input.inputValue()).length]);
  await page.getByRole('button', { name: 'Copy payment address' }).click();
  await expect(page.locator('#copy-address')).toHaveAttribute('aria-label', /Payment address copied|Could not copy/);
  for (const width of [320, 700, 1280]) {
    await page.setViewportSize({ width, height: 900 });
    const geometry = await page.evaluate(() => ({ scrollWidth: document.documentElement.scrollWidth, viewport: innerWidth,
      overflows: Array.from(document.querySelectorAll('*')).filter(el => el.getBoundingClientRect().right > innerWidth + 1)
        .slice(0, 12).map(el => [el.tagName, el.id, el.className, Math.round(el.getBoundingClientRect().right)]) }));
    expect(geometry.scrollWidth, JSON.stringify(geometry)).toBeLessThanOrEqual(geometry.viewport + 1);
  }
});

test('real compact checkout positions refund controls below a full width field', async ({ page, request }) => {
  const url = await checkoutUrl(request);
  await page.setViewportSize({ width: 375, height: 800 });
  await page.goto(`${url}?view=compact`);
  const input = await page.locator('#refund_address').boundingBox();
  const controls = await page.locator('.refund-tools').boundingBox();
  const form = await page.locator('#refund-form').boundingBox();
  expect(input.width).toBeGreaterThan(form.width * .8);
  expect(controls.y).toBeGreaterThanOrEqual(input.y + input.height - 1);
});

test('real checkout retains manual refund entry without JavaScript', async ({ browser, request }) => {
  const url = await checkoutUrl(request);
  const context = await browser.newContext({ javaScriptEnabled: false });
  try {
    const page = await context.newPage();
    await page.goto(url);
    await expect(page.locator('#refund_address')).toBeVisible();
    await expect(page.getByRole('button', { name: 'Save' })).toBeVisible();
    await expect(page.locator('#checkout-refresh')).toHaveCount(1);
  } finally { await context.close(); }
});
