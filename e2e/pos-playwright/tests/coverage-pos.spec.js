const { test, expect } = require('../coverage-test');
const { startCoverageFixture, stopCoverageFixture, serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');

let fixture;
test.beforeEach(async ({ context }) => {
  fixture = await startCoverageFixture();
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
  await context.addCookies([{ name: 'session', value: fixture.session, url: fixture.base_url }]);
});
test.afterEach(async () => { await stopCoverageFixture(fixture?.process); });

function posUrl() { return `${fixture.base_url}/dashboard/stores/${fixture.connection_id}/pos`; }

test('real POS backgrounds, reloads, reopens, cancels, and searches an order', async ({ page }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-stack-card')).toContainText('Fixture order');
  await captureCoverageStage(page, 'pos-background-stack', test.info());
  await page.reload();
  await expect(page.locator('.pos-stack-card')).toBeVisible();
  await page.locator('.pos-stack-card').click();
  await expect(page.frameLocator('.pos-checkout-card iframe').locator('#checkout-root')).toBeVisible();
  page.once('dialog', dialog => dialog.accept());
  await page.getByRole('button', { name: 'Cancel order' }).click();
  await expect(page.locator('.pos-order-heading .pos-badge')).toContainText('Cancelled');
  await captureCoverageStage(page, 'pos-cancelled', test.info());
  await expect(page.locator('.pos-checkout-card iframe')).toBeHidden();
  await page.getByRole('button', { name: 'New order' }).click();
  await page.getByRole('button', { name: 'View all →' }).click();
  await page.getByRole('tab', { name: /Finished/ }).click();
  await expect(page.locator('.pos-order-card .pos-badge')).toContainText('Cancelled');
  await page.getByRole('searchbox', { name: 'Search reference or order ID' }).fill('fixture');
  await expect(page.locator('.pos-order-card')).toContainText('Fixture order');
});

test('real POS header remains above its compact checkout frame', async ({ page }) => {
  await page.goto(posUrl());
  await expect(page.locator('.pos-checkout-card iframe')).toBeVisible();
  for (const { width, height } of [{ width: 1126, height: 700 }, { width: 667, height: 375 }, { width: 360, height: 740 }]) {
    await page.setViewportSize({ width, height });
    const bounds = await page.evaluate(() => {
      const bar = document.querySelector('.pos-top').getBoundingClientRect();
      const panel = document.querySelector('.pos-checkout-card').getBoundingClientRect();
      const frame = document.querySelector('.pos-checkout-card iframe').getBoundingClientRect();
      return { barBottom: bar.bottom, panelTop: panel.top, frameTop: frame.top,
        frameBottom: frame.bottom, viewportHeight: innerHeight };
    });
    expect(bounds.panelTop).toBeGreaterThanOrEqual(bounds.barBottom);
    expect(bounds.frameTop).toBeGreaterThanOrEqual(bounds.barBottom);
    if (height >= 620) expect(bounds.frameBottom).toBeLessThanOrEqual(bounds.viewportHeight);
    else {
      await page.getByRole('button', { name: 'Cancel order' }).scrollIntoViewIfNeeded();
      await expect(page.getByRole('button', { name: 'Cancel order' })).toBeInViewport();
    }
  }
});

test('real POS displays a shortened ID for an order without a reference', async ({ page }) => {
  await page.goto(posUrl());
  await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await page.getByRole('button', { name: '1', exact: true }).click();
  await expect(page.locator('#pos-reference')).toHaveValue('');
  await page.getByRole('button', { name: 'Charge' }).click();
  await expect(page.locator('.pos-order-heading h1')).toHaveText(/^order_…/);
  await expect(page.frameLocator('.pos-checkout-card iframe').locator('#checkout-root')).toBeVisible();
});

test('real POS opens an empty keypad when its order list is empty', async ({ page }) => {
  await page.route('**/pos/orders?*', route => route.fulfill({ json: { orders: [], total: 0 } }));
  await page.goto(posUrl());
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await expect(page.locator('.pos-stack-card')).toHaveCount(0);
  await expect(page.getByRole('button', { name: 'Charge' })).toBeDisabled();
});

test('real POS shows pending, partial, confirming, and terminal badge symbols', async ({ page }) => {
  const states = [
    ['pending', 'pending'], ['unconfirmed', 'unconfirmed'], ['confirming', 'confirming'],
    ['partial', 'partial'], ['paid', 'paid'], ['overpaid', 'overpaid'], ['expired', 'expired'],
    ['double', 'pending'], ['cancelled', 'pending'],
  ];
  const orders = states.map(([id, status], index) => ({
    order_id: id, merchant_order_id: id, address: '86hiL7n5RcVJJKBztLP1UFjCSXJZTSa276LaNaXcQuw1ZcauZJShLbB61YabbizKYVB3jHh7K3s1GCLwLVs6AwMX9FGCnfC',
    amount: '1.00', currency: 'XMR', xmr_amount: '1.000000000000', status,
    confirmations: id === 'confirming' ? 3 : 0, confirmations_required: 10,
    error: id === 'double' ? 'Double-spend detected on this payment.' : null,
    backgrounded: true, cancelled_at: id === 'cancelled' ? 2000 : null,
    created_at: 1000 + index, expires_at: 9999999999, updated_at: 1000 + index,
  }));
  await page.addInitScript(() => { window.EventSource = class { addEventListener() {} close() {} }; });
  await page.route('**/pos/orders?*', route => route.fulfill({ json: { orders, total: orders.length } }));
  await page.goto(posUrl());
  await expect(page.locator('.pos-stack-card')).toHaveCount(5);
  await expect(page.locator('.state-partial .pos-icon use')).toHaveAttribute('href', '#pos-coin-partial');
  await expect(page.locator('.state-confirming .pos-disc')).toHaveAttribute('style', /--progress: 30%/);
  await page.getByRole('button', { name: 'View all →' }).click();
  await expect(page.locator('.pos-order-card .pos-badge.state-partial use')).toHaveAttribute('href', '#pos-coin-partial');
  await page.getByRole('tab', { name: /Finished/ }).click();
  for (const name of ['paid', 'overpaid', 'expired', 'cancelled']) {
    await expect(page.locator(`.pos-order-card .pos-badge.state-${name}`)).toHaveCount(1);
  }
});

test('real dashboard health indicator follows healthy and unavailable polls', async ({ page }) => {
  let health = true;
  let polls = 0;
  let releaseFirst;
  await page.clock.install();
  await page.route('**/status/summary', async route => {
    polls++;
    if (polls === 1) await new Promise(resolve => { releaseFirst = resolve; });
    return health === null
      ? route.fulfill({ status: 503, contentType: 'text/plain', body: 'Unavailable' })
      : route.fulfill({ json: { healthy: health } });
  });
  await page.goto(`${fixture.base_url}/dashboard`);
  const dot = page.locator('#status-indicator .status-dot');
  await expect(dot).toBeVisible();
  await expect(dot).toHaveClass(/status-dot-unknown/);
  await expect.poll(() => polls).toBeGreaterThan(0);
  releaseFirst();
  await expect(dot).toHaveClass(/status-dot-ok/);
  health = null;
  await page.clock.runFor(30000);
  await expect(dot).toHaveClass(/status-dot-unknown/);
  await expect(page.locator('#status-indicator')).toHaveAttribute('title', 'could not check status');
});
