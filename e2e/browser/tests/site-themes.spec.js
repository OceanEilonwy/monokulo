// @ts-check
// The theme against the real binaries: the merchant's choice reaches the
// site, the POS and the hosted payment page that frames their checkout,
// and a checkout opened on its own takes its theme from `?theme=` or from
// the embedding site's option. Also captures the dashboard pages and the
// hosted payment page for the gallery, in both themes.
const { test, expect } = require('../coverage-test');
const { serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { useRealStack, fixture, signInAsAdmin, connectStore } = require('./backend-helpers');

useRealStack(test);

test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

/** A connected store's dashboard path: one an earlier spec connected, or a
 * new one on a fresh instance. Reusing one keeps this spec off the engine's
 * admin rate limit, which the specs before it use heavily. */
async function aStore(page, base) {
  await page.goto(base + '/');
  const existing = page.locator('a[data-store-link]').first();
  if (await existing.count()) return existing.getAttribute('href');
  return connectStore(page, 'theme.example.com');
}

/** Chooses `theme` with the account menu's switch, as a merchant does. */
async function chooseTheme(page, base, theme) {
  await page.goto(base + '/');
  await page.locator('.site-nav .acct > summary').click();
  await page.locator('.acct-menu').getByRole('button', { name: `${theme[0].toUpperCase()}${theme.slice(1)} theme` }).click();
  if (theme === 'system') await expect(page.locator('html')).not.toHaveAttribute('data-theme', /./);
  else await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
}

test('the site, the POS and the hosted payment page share one theme, light and dark', async ({ page }) => {
  test.setTimeout(4 * 60 * 1000);
  const base = fixture().monokulo_url;
  await signInAsAdmin(page);
  const store = await aStore(page, base);

  // An order to show on its own page and the hosted payment page.
  await page.goto(base + store + '/pos');
  // A reused store's POS may reopen an order an earlier spec left open.
  await expect(page.locator('.pos-keypad').or(page.locator('.pos-payment'))).toBeVisible();
  if (await page.locator('.pos-payment').isVisible()) await page.getByRole('button', { name: 'Background order', exact: true }).click();
  await expect(page.locator('.pos-keypad')).toBeVisible();
  await page.keyboard.type('420000000000');
  await page.locator('#pos-reference').fill('Table 4');
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.url().endsWith('/pos/orders') && r.request().method() === 'POST'),
    page.getByRole('button', { name: 'Charge' }).click(),
  ]);
  expect(response.ok()).toBeTruthy();
  const orderId = (await response.json()).order_id;
  const checkout = `${base}/checkout/${orderId}`;

  try {
    for (const theme of /** @type {const} */ (['light', 'dark'])) {
      await chooseTheme(page, base, theme);
      if (theme === 'light') await captureCoverageStage(page, 'site-dashboard', test.info(), { group: 'site' });
      for (const [path, stage] of [['/account', 'site-account'], [store, 'site-store-page'], [store + '/settings?tab=general', 'site-store-settings'], [`${store}/orders/${orderId}`, 'site-order-detail']]) {
        await page.goto(base + path);
        await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
        if (theme === 'light') await captureCoverageStage(page, stage, test.info(), { group: 'site' });
      }

      // The POS is in it too, its top bar the site's app bar.
      await page.goto(base + store + '/pos');
      await expect(page.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(page.locator('.pos-top .pos-brand svg.logo-mark')).toBeVisible();

      // The hosted payment page frames the checkout in the merchant's theme.
      await page.goto(`${checkout}/share`);
      await expect(page.locator('#checkout-frame')).toHaveAttribute('src', `/checkout/${orderId}?theme=${theme}`);
      const frame = page.frameLocator('#checkout-frame');
      await expect(frame.locator('#checkout-root')).toBeVisible();
      await expect(frame.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(frame.locator('#status-badge')).toHaveClass(/state-pending/);
      if (theme === 'light') await captureCoverageStage(page, 'hosted-payment-page', test.info(), { group: 'hosted-payment' });
    }
  } finally {
    await chooseTheme(page, base, 'system');
  }
  // Following the device, the hosted payment page names no theme.
  await page.goto(`${checkout}/share`);
  await expect(page.locator('#checkout-frame')).toHaveAttribute('src', `/checkout/${orderId}`);

  // A checkout on its own takes `?theme=`, and its form and live stream
  // keep it; anything but light or dark follows the device.
  const streams = [];
  page.on('request', (sent) => { if (sent.url().includes('/events?')) streams.push(sent.url()); });
  await page.goto(`${checkout}?theme=dark`);
  await expect(page.locator('html')).toHaveAttribute('data-theme', 'dark');
  await expect(page.locator('#refund-form')).toHaveAttribute('action', /theme=dark/);
  await expect.poll(() => streams.some((url) => url.includes('theme=dark'))).toBe(true);
  await page.goto(`${checkout}?theme=neon`);
  await expect(page.locator('html')).not.toHaveAttribute('data-theme', /./);

  // An integrator's page picks it with the embed option. No public key:
  // framing an order from an earlier visit needs only where it is.
  await page.goto(`${base.replace('127.0.0.1', 'localhost')}/login`);
  await page.setContent('<div id="pay"></div>');
  await page.addScriptTag({ url: `${base}/static/monokulo-client.js` });
  await page.evaluate(({ monokulo, order }) => {
    // @ts-ignore: the library's global
    window.Monokulo.mount('#pay', order, { monokulo, theme: 'light' });
  }, { monokulo: base, order: orderId });
  await expect(page.locator('#pay iframe')).toHaveAttribute('src', `${checkout}?theme=light`);
  await expect(page.frameLocator('#pay iframe').locator('html')).toHaveAttribute('data-theme', 'light');
});
