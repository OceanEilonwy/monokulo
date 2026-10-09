// @ts-check
// The theme against the real binaries: in the light and the dark theme the
// site's app bar, table headers and buttons use the theme's surfaces (never
// the old fixed black), the POS top bar is the same bar as the site nav, and
// the hosted payment page's card is the same surface as the checkout framed
// in it, so the frame's edge doesn't show. The dashboard pages fit a 320px
// phone without scrolling sideways. Also captures the dashboard pages and
// the hosted payment page for the gallery, in both themes.
const { test, expect } = require('../coverage-test');
const { serveInstrumentedAssets } = require('../coverage-fixture');
const { captureCoverageStage } = require('../coverage-screenshot');
const { useRealStack, fixture, signInAsAdmin, connectStore } = require('./backend-helpers');

useRealStack(test);

test.beforeEach(async ({ context }) => {
  if (process.env.COVERAGE_INSTRUMENT === '1') await serveInstrumentedAssets(context);
});

// views/theme.css, as the browser reports them.
const EXPECTED = {
  light: { bar: 'rgb(255, 255, 255)', tableHead: 'rgb(239, 234, 224)', button: 'rgb(255, 255, 255)' },
  dark: { bar: 'rgb(24, 24, 24)', tableHead: 'rgb(51, 51, 51)', button: 'rgb(42, 42, 42)' },
};
const ORANGE = 'rgb(255, 102, 0)';

const background = (locator) => locator.evaluate((el) => getComputedStyle(el).backgroundColor);

/** A connected store's dashboard path: one an earlier spec connected, or a
 * new one on a fresh instance. Reusing one keeps this spec off the engine's
 * admin rate limit, which the specs before it use heavily. */
async function aStore(page, base) {
  await page.goto(base + '/');
  const existing = page.getByRole('link', { name: 'view →' }).first();
  if (await existing.count()) return existing.getAttribute('href');
  return connectStore(page, 'theme.example.com');
}

/** Whether the page fits a 320px phone: its tables may scroll inside their
 * own box, but the page itself never scrolls sideways. */
async function fitsSmallPhone(page) {
  const size = page.viewportSize();
  await page.setViewportSize({ width: 320, height: 568 });
  const overflow = await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth);
  await page.setViewportSize(size);
  return overflow;
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
  const publicKey = await page.locator('#pos-root').getAttribute('data-public-key');
  await page.keyboard.type('420000000000');
  await page.locator('#pos-reference').fill('Table 4');
  const [response] = await Promise.all([
    page.waitForResponse((r) => r.url().endsWith('/pos/orders') && r.request().method() === 'POST'),
    page.getByRole('button', { name: 'Charge' }).click(),
  ]);
  expect(response.ok()).toBeTruthy();
  const orderId = (await response.json()).order_id;

  try {
    for (const theme of /** @type {const} */ (['light', 'dark'])) {
      const expected = EXPECTED[theme];
      await chooseTheme(page, base, theme);
      expect(await background(page.locator('.site-nav'))).toBe(expected.bar);
      if (theme === 'light') await captureCoverageStage(page, 'site-dashboard', test.info(), { group: 'site' });
      expect(await fitsSmallPhone(page)).toBe(0);

      await page.goto(base + '/account');
      await expect(page.getByRole('heading', { name: 'Account', level: 1 })).toBeVisible();
      if (theme === 'light') await captureCoverageStage(page, 'site-account', test.info(), { group: 'site' });
      expect(await fitsSmallPhone(page)).toBe(0);

      await page.goto(base + store);
      expect(await background(page.locator('.orders-table th, table th').first())).toBe(expected.tableHead);
      if (theme === 'light') await captureCoverageStage(page, 'site-store-page', test.info(), { group: 'site' });
      expect(await fitsSmallPhone(page)).toBe(0);

      // Buttons are neutral; the one creating action of a form is orange.
      await page.goto(base + store + '/settings');
      await page.getByRole('button', { name: 'Edit base currency', exact: true }).click();
      expect(await background(page.getByRole('button', { name: 'Update', exact: true }))).toBe(expected.button);
      await page.keyboard.press('Escape');
      await page.getByRole('button', { name: 'Edit webhooks', exact: true }).click();
      expect(await background(page.getByRole('button', { name: 'Add webhook' }))).toBe(ORANGE);
      await page.keyboard.press('Escape');
      if (theme === 'light') await captureCoverageStage(page, 'site-store-settings', test.info(), { group: 'site' });
      expect(await fitsSmallPhone(page)).toBe(0);

      await page.goto(`${base}${store}/orders/${orderId}`);
      await expect(page.locator('.kv-table .tag.state-pending .label-long')).toHaveText('Waiting for payment');
      // The order's live stream can swap the table while it's read, so poll.
      await expect.poll(() => background(page.locator('.kv-table th').first())).toBe(expected.tableHead);
      if (theme === 'light') await captureCoverageStage(page, 'site-order-detail', test.info(), { group: 'site' });
      expect(await fitsSmallPhone(page)).toBe(0);

      // The POS top bar is the site's app bar: same surface, the same mark.
      await page.goto(base + store + '/pos');
      await expect(page.locator('.pos-top .pos-brand svg.logo-mark')).toBeVisible();
      await expect(page.locator('.pos-top .pos-mode')).toHaveText('POS');
      expect(await background(page.locator('.pos-top'))).toBe(expected.bar);

      // The hosted payment page: the card and the framed checkout are one
      // surface, in the theme the merchant chose.
      await page.goto(`${base}/pay/${publicKey}/orders/${orderId}/share`);
      const frame = page.frameLocator('#checkout-frame');
      await expect(frame.locator('#checkout-root')).toBeVisible();
      await expect(frame.locator('html')).toHaveAttribute('data-theme', theme);
      await expect(frame.locator('#status-badge')).toHaveClass(/state-pending/);
      expect(await background(page.locator('.share-card'))).toBe(await background(frame.locator('body')));
      if (theme === 'light') await captureCoverageStage(page, 'hosted-payment-page', test.info(), { group: 'hosted-payment' });
    }
  } finally {
    await chooseTheme(page, base, 'system');
  }
});
